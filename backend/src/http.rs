//! HTTP/1.1 over the engine's Unix socket. Every request opens its own
//! connection and sends `Connection: close`, so a body ends with its framing
//! or with the connection. Docker answers lists with `Content-Length` and
//! streams logs, events, and pulls chunked.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

/// The longest status or header line accepted.
const MAXIMUM_LINE: u64 = 64 << 10;

pub struct Response {
    pub status: u16,
    pub body: Body,
}

pub struct Body {
    reader: BufReader<UnixStream>,
    framing: Framing,
}

enum Framing {
    Length(u64),
    Chunked { remaining: u64, started: bool, done: bool },
    Eof,
}

/// Sends one request on `stream` and reads the response head.
pub fn send(stream: UnixStream, method: &str, target: &str, body: Option<&[u8]>) -> io::Result<Response> {
    send_reader(
        stream,
        method,
        target,
        &mut body.unwrap_or_default(),
        body.unwrap_or_default().len() as u64,
        "application/json",
    )
}

/// Streams a binary archive without buffering it or JSON encoding its bytes.
pub fn send_reader(
    stream: UnixStream,
    method: &str,
    target: &str,
    body: &mut dyn Read,
    length: u64,
    content_type: &str,
) -> io::Result<Response> {
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: docker\r\nUser-Agent: lithe-docker/{}\r\nConnection: close\r\n",
        env!("CARGO_PKG_VERSION")
    );
    head.push_str(&format!("Content-Type: {content_type}\r\nContent-Length: {length}\r\n"));
    head.push_str("\r\n");
    let mut writer = &stream;
    writer.write_all(head.as_bytes())?;
    if io::copy(&mut body.take(length), &mut writer)? != length {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    writer.flush()?;

    let mut reader = BufReader::new(stream);
    let mut status = read_status(&mut reader)?;
    // An interim answer is followed by the real one.
    while (100..200).contains(&status) {
        read_headers(&mut reader)?;
        status = read_status(&mut reader)?;
    }
    let headers = read_headers(&mut reader)?;
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    let framing = if header("Transfer-Encoding").is_some_and(|value| {
        value
            .rsplit(',')
            .next()
            .is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked"))
    }) {
        Framing::Chunked {
            remaining: 0,
            started: false,
            done: false,
        }
    } else if let Some(length) = header("Content-Length") {
        Framing::Length(length.trim().parse().map_err(|_| invalid("bad Content-Length"))?)
    } else if status == 204 || status == 304 || method == "HEAD" {
        Framing::Length(0)
    } else {
        Framing::Eof
    };
    Ok(Response {
        status,
        body: Body { reader, framing },
    })
}

impl Body {
    /// The whole body, refusing one longer than `limit` bytes.
    pub fn read_all(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(invalid("the response is too large"));
        }
        Ok(bytes)
    }
}

impl Read for Body {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let reader = &mut self.reader;
        match &mut self.framing {
            Framing::Eof => reader.read(buffer),
            Framing::Length(remaining) => {
                if *remaining == 0 {
                    return Ok(0);
                }
                let wanted = buffer.len().min(usize::try_from(*remaining).unwrap_or(usize::MAX));
                let read = reader.read(&mut buffer[..wanted])?;
                if read == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                *remaining -= read as u64;
                Ok(read)
            }
            Framing::Chunked {
                remaining,
                started,
                done,
            } => {
                if *done {
                    return Ok(0);
                }
                if *remaining == 0 {
                    if *started && !read_line(reader)?.is_empty() {
                        return Err(invalid("a chunk does not end with CRLF"));
                    }
                    *started = true;
                    let line = read_line(reader)?;
                    let size = line.split(';').next().unwrap_or_default().trim();
                    let size = u64::from_str_radix(size, 16).map_err(|_| invalid("bad chunk size"))?;
                    if size == 0 {
                        while !read_line(reader)?.is_empty() {}
                        *done = true;
                        return Ok(0);
                    }
                    *remaining = size;
                }
                let wanted = buffer.len().min(usize::try_from(*remaining).unwrap_or(usize::MAX));
                let read = reader.read(&mut buffer[..wanted])?;
                if read == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                *remaining -= read as u64;
                Ok(read)
            }
        }
    }
}

fn read_status(reader: &mut BufReader<UnixStream>) -> io::Result<u16> {
    let line = read_line(reader)?;
    let mut parts = line.splitn(3, ' ');
    match (parts.next(), parts.next()) {
        (Some(version), Some(code)) if version.starts_with("HTTP/1.") => {
            code.parse().map_err(|_| invalid("bad status code"))
        }
        _ => Err(invalid("the engine did not answer HTTP")),
    }
}

fn read_headers(reader: &mut BufReader<UnixStream>) -> io::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            return Ok(headers);
        }
        if headers.len() >= 256 {
            return Err(invalid("too many headers"));
        }
        let (name, value) = line.split_once(':').ok_or_else(|| invalid("bad header"))?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
}

/// One CRLF- or LF-terminated line, without its terminator.
fn read_line(reader: &mut BufReader<UnixStream>) -> io::Result<String> {
    let mut line = Vec::new();
    reader.by_ref().take(MAXIMUM_LINE).read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        return Err(if line.len() as u64 >= MAXIMUM_LINE {
            invalid("a line is too long")
        } else {
            io::ErrorKind::UnexpectedEof.into()
        });
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| invalid("a header is not UTF-8"))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Which output of a container a log line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Stdout,
    Stderr,
}

/// Lines of a container's log stream. Without a TTY, Docker multiplexes
/// stdout and stderr into frames of an 8-byte header (stream, three zero
/// bytes, big-endian length) and the payload; with one, it sends the raw
/// terminal output.
pub struct LogLines<R> {
    reader: BufReader<R>,
    multiplexed: bool,
    pending: std::collections::VecDeque<(Output, Vec<u8>)>,
    partial: [Vec<u8>; 2],
    discarding: [bool; 2],
    maximum_line: usize,
}

impl<R: Read> LogLines<R> {
    pub fn new(reader: R, multiplexed: bool, maximum_line: usize) -> Self {
        Self {
            reader: BufReader::new(reader),
            multiplexed,
            pending: Default::default(),
            partial: Default::default(),
            discarding: [false; 2],
            maximum_line,
        }
    }

    /// The next line without its line break, cut to the maximum line
    /// length, or `None` at the end of the stream.
    pub fn next_line(&mut self) -> io::Result<Option<(Output, Vec<u8>)>> {
        if !self.multiplexed {
            let mut line = Vec::new();
            let read = (&mut self.reader)
                .take(self.maximum_line as u64)
                .read_until(b'\n', &mut line)?;
            if read == 0 {
                return Ok(None);
            }
            if line.last() == Some(&b'\n') {
                line.pop();
            } else if line.len() >= self.maximum_line {
                // Skip the rest of an overlong line.
                loop {
                    let available = self.reader.fill_buf()?;
                    let end = available.iter().position(|byte| *byte == b'\n');
                    let length = end.map_or(available.len(), |index| index + 1);
                    self.reader.consume(length);
                    if end.is_some() || length == 0 {
                        break;
                    }
                }
            }
            trim_carriage_return(&mut line);
            return Ok(Some((Output::Stdout, line)));
        }
        while self.pending.is_empty() {
            let mut header = [0u8; 8];
            match self.reader.read(&mut header[..1])? {
                0 => {
                    for (index, output) in [Output::Stdout, Output::Stderr].into_iter().enumerate() {
                        if !self.partial[index].is_empty() {
                            self.pending
                                .push_back((output, std::mem::take(&mut self.partial[index])));
                        }
                    }
                    return Ok(self.pending.pop_front());
                }
                _ => self.reader.read_exact(&mut header[1..])?,
            }
            if !matches!(header[0], 1 | 2) || header[1..4] != [0, 0, 0] {
                return Err(invalid("bad Docker log frame"));
            }
            let output = if header[0] == 2 { Output::Stderr } else { Output::Stdout };
            let index = usize::from(header[0] == 2);
            let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
            if length > 4 << 20 {
                return Err(invalid("the Docker log frame is too large"));
            }
            let mut payload = vec![0u8; length];
            self.reader.read_exact(&mut payload)?;
            for byte in payload {
                if byte == b'\n' {
                    let mut line = std::mem::take(&mut self.partial[index]);
                    trim_carriage_return(&mut line);
                    self.pending.push_back((output, line));
                    self.discarding[index] = false;
                } else if !self.discarding[index] {
                    if self.partial[index].len() < self.maximum_line {
                        self.partial[index].push(byte);
                    } else {
                        self.discarding[index] = true;
                    }
                }
            }
        }
        Ok(self.pending.pop_front())
    }
}

fn trim_carriage_return(line: &mut Vec<u8>) {
    while line.last() == Some(&b'\r') {
        line.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn serve(answer: &'static [u8]) -> Response {
        let (client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        client
            .set_write_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        server
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        server
            .set_write_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let (finished, completion) = std::sync::mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let mut request = Vec::new();
            let mut reader = BufReader::new(&server);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                request.extend_from_slice(line.as_bytes());
                if line == "\r\n" {
                    break;
                }
            }
            (&server).write_all(answer).unwrap();
            finished.send(()).unwrap();
            String::from_utf8(request).unwrap()
        });
        let response = send(client, "GET", "/version", None).unwrap();
        completion.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /version HTTP/1.1\r\nHost: docker\r\n"),
            "{request}"
        );
        assert!(request.contains("Connection: close\r\n"));
        response
    }

    #[test]
    fn reads_a_body_by_its_length() {
        let mut response = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello trailing");
        assert_eq!(response.status, 200);
        assert_eq!(response.body.read_all(1024).unwrap(), b"hello");
    }

    #[test]
    fn decodes_chunked_bodies() {
        let mut response = serve(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n6;ext=1\r\npedia \r\nE\r\nin \r\n\r\nchunks.\r\n0\r\nTrailer: x\r\n\r\n",
        );
        assert_eq!(
            String::from_utf8(response.body.read_all(1024).unwrap()).unwrap(),
            "Wikipedia in \r\n\r\nchunks."
        );
    }

    #[test]
    fn reads_to_the_end_without_framing_and_skips_interim_answers() {
        let mut response = serve(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 404 Not Found\r\n\r\n{}");
        assert_eq!(response.status, 404);
        assert_eq!(response.body.read_all(1024).unwrap(), b"{}");
    }

    #[test]
    fn refuses_oversized_bodies() {
        let mut response = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789");
        assert!(response.body.read_all(4).is_err());
    }

    #[test]
    fn splits_multiplexed_log_frames() {
        let mut stream = Vec::new();
        for (kind, text) in [(1u8, &b"one\ntwo\n"[..]), (2, b"oops\r\n"), (1, b"three")] {
            stream.extend_from_slice(&[kind, 0, 0, 0]);
            stream.extend_from_slice(&(text.len() as u32).to_be_bytes());
            stream.extend_from_slice(text);
        }
        let mut lines = LogLines::new(&stream[..], true, 1024);
        let mut seen = Vec::new();
        while let Some((output, line)) = lines.next_line().unwrap() {
            seen.push((output, String::from_utf8(line).unwrap()));
        }
        assert_eq!(
            seen,
            [
                (Output::Stdout, "one".to_string()),
                (Output::Stdout, "two".to_string()),
                (Output::Stderr, "oops".to_string()),
                (Output::Stdout, "three".to_string()),
            ]
        );
    }

    #[test]
    fn reassembles_lines_across_frames_without_merging_output_streams() {
        let mut stream = Vec::new();
        for (kind, text) in [(1u8, &b"hel"[..]), (2, b"error\n"), (1, b"lo\nlast")] {
            stream.extend_from_slice(&[kind, 0, 0, 0]);
            stream.extend_from_slice(&(text.len() as u32).to_be_bytes());
            stream.extend_from_slice(text);
        }
        let mut lines = LogLines::new(&stream[..], true, 1024);
        assert_eq!(lines.next_line().unwrap(), Some((Output::Stderr, b"error".to_vec())));
        assert_eq!(lines.next_line().unwrap(), Some((Output::Stdout, b"hello".to_vec())));
        assert_eq!(lines.next_line().unwrap(), Some((Output::Stdout, b"last".to_vec())));
        assert_eq!(lines.next_line().unwrap(), None);
    }

    #[test]
    fn reads_raw_terminal_lines_and_cuts_long_ones() {
        let mut lines = LogLines::new(&b"short\r\nabcdefghij\nlast"[..], false, 4);
        let mut seen = Vec::new();
        while let Some((_, line)) = lines.next_line().unwrap() {
            seen.push(String::from_utf8(line).unwrap());
        }
        assert_eq!(seen, ["shor", "abcd", "last"]);
    }
}
