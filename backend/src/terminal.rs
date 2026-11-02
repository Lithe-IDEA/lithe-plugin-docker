//! Container shells use API-24's native terminal service, never a web PTY or
//! shell command injection. Handles and launch history remain host-owned and
//! memory-only; explicit socket/argv keep each shell on its selected engine.

use crate::docker::{encode, Docker};
use lithe_plugin_sdk::{Context, FunctionError, PluginError, SdkError, Value, WorkspaceContext};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, TryLockError};

const PREFIX: &str = "lithe/host/terminal/";
const SHELLS: &[&str] = &["/bin/sh", "/bin/bash", "/bin/ash"];
const HISTORY_BYTES: usize = 16 * 1024;

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    scope: WorkspaceContext,
    socket: PathBuf,
    container: String,
    shell: String,
}

#[derive(Default)]
pub struct Terminals {
    // One operation at a time, with an immediate busy error instead of blocking
    // SDK workers while another operation awaits the user's native consent.
    handles: Mutex<HashMap<Key, String>>,
}

impl Terminals {
    pub fn open(&self, docker: &Docker, params: &Value, context: &Context) -> Result<Value, FunctionError> {
        let key = key(&docker.socket, params, context)?;
        for method in ["create", "show", "reopen", "dispose"] {
            if !context
                .host()
                .supports(&format!("{PREFIX}{method}"))
                .map_err(host_error)?
            {
                return Err(error(
                    "docker.terminalUnavailable",
                    "This host does not provide native terminals.",
                ));
            }
        }
        let mut handles = self.handles()?;
        // The host releases the old generation's terminals on context changes.
        handles
            .retain(|existing, _| existing.scope.context_id() != key.scope.context_id() || existing.scope == key.scope);
        let inspected = docker.get(context, &format!("/containers/{}/json", encode(&key.container)))?;
        if inspected["State"]["Running"] != true || inspected["State"]["Paused"] == true {
            return Err(error(
                "docker.terminalStopped",
                "Start or resume this container before opening a terminal.",
            ));
        }
        context.check_cancelled()?;
        let host = context.host();
        if let Some(id) = handles.get(&key) {
            let request = json!({ "context": key.scope.to_value(), "terminalID": id });
            match host.request(&format!("{PREFIX}show"), request.clone()) {
                Ok(_) => return Ok(json!({ "terminalID": id })),
                Err(failure) if failure.code() == "plugin.host.preconditionFailed" => {
                    // Reopen is explicit and still asks the host for execution
                    // consent. Never retry a cancelled create/reopen.
                    return host.request(&format!("{PREFIX}reopen"), request).map_err(host_error);
                }
                Err(failure) if failure.code() == "plugin.host.notFound" => {
                    handles.remove(&key);
                }
                Err(failure) => return Err(host_error(failure)),
            }
        }
        // Inspect supplies the canonical ID: a caller's name beginning with '-'
        // must never become another CLI option. The UI supplies only the title.
        let container = inspected["Id"]
            .as_str()
            .filter(|id| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| error("docker.terminalInvalid", "Docker did not return a valid container ID."))?;
        let title = params["title"]
            .as_str()
            .ok_or_else(|| error("docker.invalidParams", "A localized terminal title is required."))?;
        let launch = launch(&key.socket, container, &key.shell, title, &key.scope)?;
        let created = host.request(&format!("{PREFIX}create"), launch).map_err(host_error)?;
        let id = created["terminalID"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                error(
                    "docker.terminalInvalid",
                    "The native terminal service returned no handle.",
                )
            })?;
        handles.insert(key, id.to_string());
        Ok(created)
    }

    pub fn dispose(&self, socket: &Path, params: &Value, context: &Context) -> Result<Value, FunctionError> {
        let key = key(socket, params, context)?;
        let mut handles = self.handles()?;
        if let Some(id) = handles.get(&key) {
            let request = json!({ "context": key.scope.to_value(), "terminalID": id });
            match context.host().request(&format!("{PREFIX}dispose"), request) {
                Ok(_) => {}
                Err(failure) if failure.code() == "plugin.host.notFound" => {}
                Err(failure) => return Err(host_error(failure)),
            }
            handles.remove(&key);
        }
        Ok(Value::Null)
    }

    /// Removing a container releases closed handles too, so its no-longer-
    /// selectable row cannot permanently consume a host terminal slot.
    pub fn dispose_container(&self, socket: &Path, container: &str, context: &Context) -> Result<(), FunctionError> {
        let Some(scope) = context.workspace_context() else {
            return Ok(());
        };
        let mut handles = self.handles()?;
        let keys: Vec<_> = handles
            .keys()
            .filter(|key| key.scope == *scope && key.socket == socket && key.container == container)
            .cloned()
            .collect();
        for key in keys {
            let id = &handles[&key];
            match context.host().request(
                &format!("{PREFIX}dispose"),
                json!({
                    "context": scope.to_value(), "terminalID": id
                }),
            ) {
                Ok(_) => {}
                Err(failure) if failure.code() == "plugin.host.notFound" => {}
                Err(failure) => return Err(host_error(failure)),
            }
            handles.remove(&key);
        }
        Ok(())
    }

    fn handles(&self) -> Result<std::sync::MutexGuard<'_, HashMap<Key, String>>, FunctionError> {
        match self.handles.try_lock() {
            Ok(handles) => Ok(handles),
            Err(TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => Err(error(
                "plugin.host.busy",
                "A terminal operation is already in progress.",
            )),
        }
    }
}

fn key(socket: &Path, params: &Value, context: &Context) -> Result<Key, FunctionError> {
    let scope = context.workspace_context().ok_or_else(|| {
        error(
            "docker.terminalUnavailable",
            "Native terminals require a Lithe workspace context.",
        )
    })?;
    let container = params["id"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| error("docker.invalidParams", "A container ID is required."))?;
    let shell = params["shell"].as_str().unwrap_or("/bin/sh");
    if !SHELLS.contains(&shell) {
        return Err(error("docker.invalidParams", "Choose a supported container shell."));
    }
    Ok(Key {
        scope: scope.clone(),
        socket: socket.into(),
        container: container.into(),
        shell: shell.into(),
    })
}

fn launch(
    socket: &Path,
    container: &str,
    shell: &str,
    title: &str,
    scope: &WorkspaceContext,
) -> Result<Value, FunctionError> {
    let socket = socket
        .to_str()
        .filter(|socket| socket.starts_with('/') && !socket.contains('\0'))
        .ok_or_else(|| {
            error(
                "docker.terminalInvalid",
                "The Docker socket is not a valid absolute UTF-8 path.",
            )
        })?;
    Ok(json!({
        "context": scope.to_value(), "executable": "docker",
        "arguments": ["--host", format!("unix://{socket}"), "exec", "--interactive", "--tty", container, shell],
        "title": title, "show": true, "retainedContextBytes": HISTORY_BYTES,
        // DOCKER_CONTEXT overrides DOCKER_HOST; inherited TLS configuration can
        // also conflict with the explicitly selected local Unix socket.
        "environment": { "DOCKER_CONTEXT": null, "DOCKER_HOST": null, "DOCKER_TLS": null,
            "DOCKER_TLS_VERIFY": null, "DOCKER_CERT_PATH": null }
    }))
}

fn error(code: &str, message: &str) -> FunctionError {
    PluginError::new(code, message).into()
}

fn host_error(failure: SdkError) -> FunctionError {
    match failure.code() {
        "sdk.call.cancelled" | "requestCancelled" | "plugin.host.cancelled" => FunctionError::Cancelled,
        _ => error(failure.code(), failure.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_pins_the_engine_and_preserves_argv_without_shell_expansion() {
        let scope = WorkspaceContext::new("docker-test", 2).unwrap();
        let socket = Path::new("/tmp/docker socket;$(touch unexpected).sock");
        let request = launch(socket, &"a".repeat(64), "/bin/sh", "Docker · 容器", &scope).unwrap();
        assert_eq!(request["executable"], "docker");
        assert_eq!(
            request["arguments"],
            json!([
                "--host",
                "unix:///tmp/docker socket;$(touch unexpected).sock",
                "exec",
                "--interactive",
                "--tty",
                "a".repeat(64),
                "/bin/sh"
            ])
        );
        assert_eq!(request["context"], scope.to_value());
        assert_eq!(request["environment"]["DOCKER_CONTEXT"], Value::Null);
        assert_eq!(request["retainedContextBytes"], 16384);
        assert!(launch(Path::new("relative.sock"), "id", "/bin/sh", "Docker", &scope).is_err());
    }
}
