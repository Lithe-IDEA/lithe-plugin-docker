//! Requests to the Docker Engine API on one socket. Paths carry no API
//! version, so the engine answers with its own. Cancelling a call shuts its
//! connections down, which ends any read in progress.

use crate::http::{self, Response};
use lithe_plugin_sdk::{Context, FunctionError, PluginError};
use serde_json::Value;
use std::io;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// The largest JSON answer read; lists of thousands of objects stay far below.
const JSON_LIMIT: usize = 64 << 20;

/// How long an ordinary request may wait for the engine.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Docker {
    pub socket: PathBuf,
}

/// A response whose body is still being read, with a handle that can shut
/// its connection down from another thread.
pub struct Stream {
    pub response: Response,
    pub connection: Arc<UnixStream>,
}

impl Docker {
    /// Uploads a prepared tar stream. Cancellation closes the owned socket.
    pub fn upload_archive(
        &self,
        context: &Context,
        target: &str,
        reader: &mut dyn io::Read,
        length: u64,
    ) -> Result<(), FunctionError> {
        context.check_cancelled()?;
        let stream = UnixStream::connect(&self.socket).map_err(connect_error)?;
        stream.set_read_timeout(Some(DEFAULT_TIMEOUT))?;
        stream.set_write_timeout(Some(DEFAULT_TIMEOUT))?;
        let connection = Arc::new(stream.try_clone()?);
        let watcher = Arc::downgrade(&connection);
        context.cancellation().on_cancel(move || {
            if let Some(connection) = watcher.upgrade() {
                let _ = connection.shutdown(Shutdown::Both);
            }
        });
        let response = http::send_reader(stream, "PUT", target, reader, length, "application/x-tar")
            .map_err(|error| io_error(context, error))?;
        check(&mut Stream { response, connection }, context)
    }

    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    /// Sends a request; `timeout` bounds every read, `None` waits as long as
    /// the call lasts.
    pub fn open(
        &self,
        context: &Context,
        method: &str,
        target: &str,
        body: Option<&Value>,
        timeout: Option<Duration>,
    ) -> Result<Stream, FunctionError> {
        context.check_cancelled()?;
        let stream = UnixStream::connect(&self.socket).map_err(connect_error)?;
        stream.set_read_timeout(Some(timeout.unwrap_or(DEFAULT_TIMEOUT)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let connection = Arc::new(stream.try_clone()?);
        let watcher = Arc::downgrade(&connection);
        context.cancellation().on_cancel(move || {
            if let Some(connection) = watcher.upgrade() {
                let _ = connection.shutdown(Shutdown::Both);
            }
        });
        let body = body.map(serde_json::to_vec).transpose()?;
        let response = http::send(stream, method, target, body.as_deref()).map_err(|error| io_error(context, error))?;
        connection.set_read_timeout(timeout)?;
        Ok(Stream { response, connection })
    }

    /// A request whose JSON answer is read whole; an empty answer is `null`.
    pub fn call(
        &self,
        context: &Context,
        method: &str,
        target: &str,
        body: Option<&Value>,
    ) -> Result<Value, FunctionError> {
        self.call_with(context, method, target, body, DEFAULT_TIMEOUT)
    }

    pub fn call_with(
        &self,
        context: &Context,
        method: &str,
        target: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, FunctionError> {
        let mut stream = self.open(context, method, target, body, Some(timeout))?;
        let status = stream.response.status;
        let bytes = stream
            .response
            .body
            .read_all(JSON_LIMIT)
            .map_err(|error| io_error(context, error))?;
        if status >= 300 && status != 304 {
            return Err(status_error(status, &bytes).into());
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn get(&self, context: &Context, target: &str) -> Result<Value, FunctionError> {
        self.call(context, "GET", target, None)
    }
}

/// Fails a streaming response that did not succeed, with the engine's message.
pub fn check(stream: &mut Stream, context: &Context) -> Result<(), FunctionError> {
    let status = stream.response.status;
    if status < 300 {
        return Ok(());
    }
    let bytes = stream
        .response
        .body
        .read_all(1 << 20)
        .map_err(|error| io_error(context, error))?;
    Err(status_error(status, &bytes).into())
}

/// The engine's `{ "message" }` as a plugin error the page can tell apart.
pub fn status_error(status: u16, body: &[u8]) -> PluginError {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_string());
    let message = if message.is_empty() {
        format!("Docker answered HTTP {status}.")
    } else {
        capitalized(&message)
    };
    let code = match status {
        400 => "docker.badRequest",
        401 | 403 => "docker.forbidden",
        404 => "docker.notFound",
        409 => "docker.conflict",
        _ => "docker.failed",
    };
    PluginError::new(code, message)
}

fn capitalized(message: &str) -> String {
    let mut characters = message.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn connect_error(error: io::Error) -> FunctionError {
    match error.kind() {
        io::ErrorKind::PermissionDenied => PluginError::new(
            "docker.permissionDenied",
            "Lithe isn't allowed to use the Docker socket.",
        )
        .into(),
        _ => PluginError::new(
            "docker.unreachable",
            "Docker isn't running, or its socket can't be reached.",
        )
        .into(),
    }
}

/// A failed read or write: cancelled when the call was, else a lost connection.
pub fn io_error(context: &Context, error: io::Error) -> FunctionError {
    if context.cancellation().is_cancelled() {
        return FunctionError::Cancelled;
    }
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            PluginError::new("docker.timeout", "Docker didn't answer in time.").into()
        }
        io::ErrorKind::InvalidData => FunctionError::internal(format!("The engine's answer is malformed: {error}")),
        _ => PluginError::new("docker.connectionLost", "The connection to Docker was lost.").into(),
    }
}

/// What [`collect`] gathered.
pub struct Collected<T> {
    pub items: Vec<T>,
    /// The stream ended on its own, for example because the container stopped.
    pub ended: bool,
}

/// Long-polls a stream: reads items on a thread of its own until `wait`
/// passes, `grace` after the first item, the items weigh `budget`, or the
/// stream ends, then shuts the connection down. Items read after that are
/// dropped; callers resume from the last item they return.
pub fn collect<T, F>(
    connection: Arc<UnixStream>,
    mut next: F,
    weight: fn(&T) -> usize,
    wait: Duration,
    grace: Duration,
    budget: usize,
) -> io::Result<Collected<T>>
where
    T: Send + 'static,
    F: FnMut() -> io::Result<Option<T>> + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    let (finished, completion) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("lithe-docker-stream".into())
        .spawn(move || {
            loop {
                let item = next();
                let last = !matches!(item, Ok(Some(_)));
                if sender.send(item).is_err() || last {
                    break;
                }
            }
            let _ = finished.send(());
        })?;
    let mut deadline = Instant::now() + wait;
    let mut items = Vec::new();
    let mut weighed = 0;
    let mut ended = false;
    let mut failure = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok(Ok(Some(item))) => {
                if items.is_empty() {
                    deadline = deadline.min(Instant::now() + grace);
                }
                weighed += weight(&item);
                items.push(item);
                if weighed >= budget {
                    break;
                }
            }
            Ok(Ok(None)) | Err(RecvTimeoutError::Disconnected) => {
                ended = true;
                break;
            }
            Ok(Err(error)) => {
                ended = true;
                failure = Some(error);
                break;
            }
            Err(RecvTimeoutError::Timeout) => break,
        }
    }
    let _ = connection.shutdown(Shutdown::Both);
    drop(receiver);
    completion
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the Docker stream reader did not stop"))?;
    reader
        .join()
        .map_err(|_| io::Error::other("the Docker stream reader panicked"))?;
    match failure {
        Some(error) if items.is_empty() => Err(error),
        _ => Ok(Collected { items, ended }),
    }
}

/// Percent-encodes a path segment or query value.
pub fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_reserved_characters() {
        assert_eq!(encode("ghcr.io/me/app:1.0"), "ghcr.io%2Fme%2Fapp%3A1.0");
        assert_eq!(encode(r#"{"label":["a=b"]}"#), "%7B%22label%22%3A%5B%22a%3Db%22%5D%7D");
    }

    #[test]
    fn maps_engine_failures_to_codes() {
        let error = status_error(404, br#"{"message":"no such container: web"}"#);
        assert_eq!(
            (error.code(), error.message()),
            ("docker.notFound", "No such container: web")
        );
        assert_eq!(status_error(409, b"").message(), "Docker answered HTTP 409.");
        assert_eq!(status_error(500, b"plain text\n").message(), "Plain text");
    }

    #[test]
    fn ends_a_blocked_reader_at_the_poll_deadline() {
        use std::io::Read;
        let (connection, _peer) = UnixStream::pair().unwrap();
        let mut input = connection.try_clone().unwrap();
        input.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let collected = collect(
            Arc::new(connection),
            move || {
                let mut byte = [0];
                Ok((input.read(&mut byte)? > 0).then_some(byte[0]))
            },
            |_| 1,
            Duration::ZERO,
            Duration::ZERO,
            100,
        )
        .unwrap();
        assert!(collected.items.is_empty());
        assert!(!collected.ended);
    }

    #[test]
    fn stops_at_the_end_of_the_stream_or_the_budget() {
        let (connection, _peer) = UnixStream::pair().unwrap();
        let mut left = 3;
        let collected = collect(
            Arc::new(connection),
            move || {
                left -= 1;
                Ok((left >= 0).then_some(left))
            },
            |_| 1,
            Duration::from_secs(5),
            Duration::from_secs(5),
            100,
        )
        .unwrap();
        assert_eq!((collected.items, collected.ended), (vec![2, 1, 0], true));

        let (connection, _peer) = UnixStream::pair().unwrap();
        let collected = collect(
            Arc::new(connection),
            || Ok(Some(7)),
            |_| 10,
            Duration::from_secs(5),
            Duration::from_secs(5),
            25,
        )
        .unwrap();
        assert_eq!(collected.items, [7, 7, 7]);
    }
}
