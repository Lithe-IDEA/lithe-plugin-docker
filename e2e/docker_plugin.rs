//! Drives the packaged Docker backend through real Rust Core and Docker.
//! Uses only a uniquely named test container; existing workloads are read-only.
//! Package bytes are compared before and after calls, context persistence and restart.

use lithe_core::execute_json;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
const WAIT: Duration = Duration::from_secs(30);

#[test]
fn packaged_backend_manages_a_real_container_without_writing_the_package() {
    let package = PathBuf::from(std::env::var_os("LITHE_DOCKER_PLUGIN_PACKAGE").expect("package required"))
        .canonicalize().expect("package path");
    let manifest: Value = serde_json::from_slice(&fs::read(package.join("plugin.json")).unwrap()).unwrap();
    assert_eq!(core("plugins.declarative.validate", json!({ "manifest": manifest }))["status"], "ok");
    let before = listing(&package);
    let fixture = Fixture::new();
    let state = fixture.root.join("state");
    fs::create_dir_all(&state).unwrap();
    let plan = launch_plan(&manifest, &package, &state);
    let plugin_id = manifest["id"].as_str().unwrap();
    let mut store = BTreeMap::new();
    let mut host = Host::start(plugin_id, &package, &plan, &state);
    let functions = host.functions();
    for required in ["images.logo", "browser.open", "containers.terminal", "containers.terminalDispose", "files.list", "files.uploadBegin", "files.uploadChunk", "files.uploadCommit", "files.uploadAbort", "files.download"] {
        assert!(functions.iter().any(|function| function == required), "missing {required}");
    }
    // Verify real Core permission/capability routing without launching a
    // browser from an automated integration test. The native host owns that
    // final side effect, not this SDK transport fixture.
    assert_eq!(host.value(&mut store, "browser.open", json!({ "url": "https://example.com/lithe-docker-e2e" }))["opened"], true);
    let refused = host.call(&mut store, "browser.open", json!({ "url": "file:///etc/passwd" }));
    assert!(refused.get("error").is_some());
    let info = host.value(&mut store, "engine.info", json!({}));
    assert_eq!(info["reachable"], true, "Start Docker before running e2e: {info}");
    let socket = info["endpoint"]["socket"].clone();
    let saved = host.value(&mut store, "contexts.use", json!({ "socket": socket }));
    assert_eq!(saved["source"], "setting");
    assert!(state.join("settings.json").exists());
    for function in ["containers.list", "images.list", "volumes.list", "networks.list"] {
        assert!(host.value(&mut store, function, json!({})).is_array());
    }
    assert!(host.value(&mut store, "system.df", json!({}))["images"]["size"].is_number());
    // The cleanup session remains usable even if an assertion unwinds below.
    let name = format!("lithe-docker-e2e-{}", std::process::id());
    let image = std::env::var("LITHE_DOCKER_TEST_IMAGE").unwrap_or_else(|_| "alpine:3.21".into());
    let created = host.value(&mut store, "containers.create", json!({
        "image": image, "name": name, "command": ["sh", "-c", "echo lithe-docker-e2e; echo lithe-stderr >&2; exec sleep 120"],
        "restart": "no"
    }));
    let id = created["id"].as_str().expect("container ID").to_string();
    struct Cleanup<'a> { host: &'a mut Host, store: &'a mut BTreeMap<String, String>, id: String }
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.host.call(self.store, "containers.remove", json!({ "id": self.id, "force": true, "volumes": true }));
        }
    }
    let guard = Cleanup { host: &mut host, store: &mut store, id: id.clone() };
    // This Core fixture has no native window/PTY service. Validate the real
    // backend's API-24 request and return that actual limitation, rather than
    // inventing a successful terminal. Native interaction is host acceptance.
    let terminal = guard.host.call(guard.store, "containers.terminal", json!({
        "id": id, "shell": "/bin/sh", "title": "Docker · e2e"
    }));
    assert!(terminal["error"].to_string().contains("plugin.host.unavailable"), "{terminal}");
    assert_eq!(guard.host.terminal_requests, 1);
    let launch = &guard.host.events.iter().find(|event| event["method"] == "lithe/host/terminal/create").unwrap()["result"]["params"];
    assert_eq!(launch["arguments"][1], format!("unix://{}", socket.as_str().unwrap()));
    assert_eq!(launch["arguments"][5], id);
    let invalid_shell = guard.host.call(guard.store, "containers.terminal", json!({
        "id": id, "shell": "sh -c echo unsafe", "title": "Docker · e2e"
    }));
    assert!(invalid_shell["error"].to_string().contains("docker.invalidParams"), "{invalid_shell}");
    assert_eq!(guard.host.terminal_requests, 1, "invalid shells must not reach the host");
    let inspected = guard.host.value(guard.store, "objects.inspect", json!({ "kind": "container", "id": id }));
    assert_eq!(inspected["State"]["Running"], true);
    let logs = guard.host.value(guard.store, "containers.logs", json!({ "id": id, "tail": 100 }));
    assert!(logs["lines"].as_array().unwrap().iter().any(|line| line["text"] == "lithe-docker-e2e"), "{logs}");
    assert!(logs["lines"].as_array().unwrap().iter().any(|line| line["stream"] == "stderr"));
    assert!(guard.host.value(guard.store, "containers.stats", json!({ "id": id }))["memoryUsed"].is_number());
    assert!(guard.host.value(guard.store, "containers.top", json!({ "id": id }))["processes"].is_array());
    let directory = guard.host.value(guard.store, "files.list", json!({ "id": id, "path": "/" }));
    assert!(directory["entries"].as_array().unwrap().iter().any(|entry| entry["name"] == "tmp" && entry["kind"] == "directory"));
    let transfer = guard.host.value(guard.store, "files.uploadBegin", json!({ "id": id, "path": "/tmp", "name": "lithe-cancelled.txt", "size": 20 }))["transfer"].clone();
    guard.host.value(guard.store, "files.uploadAbort", json!({ "transfer": transfer }));
    // Exercise binary archive bytes through the same Core function boundary,
    // including non-ASCII filenames. All container writes stay in this fixture.
    let content = b"lithe-file-roundtrip\n";
    let transfer = guard.host.value(guard.store, "files.uploadBegin", json!({ "id": id, "path": "/tmp", "name": "lithe-\u{6587}\u{4ef6}.txt", "size": content.len() }))["transfer"].clone();
    guard.host.value(guard.store, "files.uploadChunk", json!({ "transfer": transfer, "offset": 0, "data": "bGl0aGUtZmlsZS1yb3VuZHRyaXAK" }));
    guard.host.value(guard.store, "files.uploadCommit", json!({ "transfer": transfer }));
    let directory = guard.host.value(guard.store, "files.list", json!({ "id": id, "path": "/tmp" }));
    assert!(directory["entries"].as_array().unwrap().iter().any(|entry| entry["name"] == "lithe-\u{6587}\u{4ef6}.txt" && entry["size"] == content.len()));
    let destination = fixture.root.join("downloaded.txt");
    guard.host.value(guard.store, "files.download", json!({ "id": id, "path": "/tmp/lithe-\u{6587}\u{4ef6}.txt", "destination": destination }));
    assert_eq!(fs::read(destination).unwrap(), content);
    let source = fixture.root.join("local-upload.txt");
    fs::write(&source, content).unwrap();
    guard.host.value(guard.store, "files.uploadLocal", json!({ "id": id, "path": "/tmp", "source": source }));
    let destination = fixture.root.join("local-download.txt");
    guard.host.value(guard.store, "files.download", json!({ "id": id, "path": "/tmp/local-upload.txt", "destination": destination }));
    assert_eq!(fs::read(destination).unwrap(), content);
    for action in ["pause", "unpause", "restart", "stop", "start", "stop"] {
        guard.host.value(guard.store, "containers.action", json!({ "id": id, "action": action }));
    }
    let stopped_terminal = guard.host.call(guard.store, "containers.terminal", json!({
        "id": id, "shell": "/bin/sh", "title": "Docker · e2e"
    }));
    assert!(stopped_terminal["error"].to_string().contains("docker.terminalStopped"), "{stopped_terminal}");
    assert_eq!(guard.host.terminal_requests, 1, "stopped containers must not launch a terminal");
    drop(guard);
    host.stop();
    drop(host);
    let mut host = Host::start(plugin_id, &package, &plan, &state);
    assert_eq!(host.value(&mut store, "contexts.list", json!({}))["choice"]["socket"], socket);
    host.value(&mut store, "contexts.use", json!({}));
    host.stop();
    drop(host);
    assert_eq!(listing(&package), before, "runtime changed the plugin package");
}

fn core(command: &str, payload: Value) -> Value {
    let request = json!({ "id": format!("docker-e2e-{command}"), "command": command, "payload": payload });
    let response: Value = serde_json::from_str(&execute_json(&request.to_string()))
        .expect("Core response should be JSON");
    assert_eq!(response["ok"], true, "{command} failed: {response}");
    response["data"].clone()
}

fn launch_plan(manifest: &Value, package: &Path, state: &Path) -> Value {
    let plan = core(
        "plugins.declarative.languageServerLaunchPlan",
        json!({
            "manifest": manifest,
            "backend": true,
            "request": {
                "pluginDirectory": package,
                "stateDirectory": state,
                "architecture": if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" },
            }
        }),
    );
    assert_eq!(plan["status"], "ok", "{plan}");
    assert_eq!(plan["plan"]["launch"]["kind"], "packaged");
    plan["plan"].clone()
}

/// Every file of a directory with its contents, to prove the package is unchanged.
fn listing(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("readable directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let contents = fs::read(&path).expect("readable file");
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), contents);
            }
        }
    }
    files
}

/// The temporary state directory and database, removed even when an assertion fails.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("lithe-docker-plugin-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory");
        // The backend sees canonical paths (macOS /var is /private/var).
        Self { root: root.canonicalize().expect("fixture path") }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One running backend session. Dropping it stops the process and removes
/// the session, so a failed assertion leaves no backend behind.
struct Host {
    session: String,
    plugin_id: String,
    events: Vec<Value>,
    answered: BTreeSet<String>,
    operations: usize,
    terminal_requests: usize,
}

impl Host {
    /// Starts the backend as the host does: the packaged executable, under
    /// the plugin's backend provider ID, with the state directory as root and
    /// working directory and the plan's host services.
    fn start(plugin_id: &str, package: &Path, plan: &Value, state: &Path) -> Self {
        let executable = package.join(plan["launch"]["path"].as_str().expect("packaged path"));
        let started = core(
            "lsp.startServer",
            json!({
                "providerId": format!("pluginBackend:{plugin_id}"),
                "pluginId": plugin_id,
                "role": "backend",
                "executablePath": executable,
                "arguments": plan["arguments"],
                "rootUri": format!("file://{}/", state.display()),
                "workingDirectory": state,
                "hostServices": plan["backend"]["hostServices"],
                "initializeTimeoutMilliseconds": WAIT.as_millis() as u64,
                "requestTimeoutMilliseconds": WAIT.as_millis() as u64,
                "shutdownTimeoutMilliseconds": 5000,
            }),
        );
        let mut host = Self {
            session: started["sessionId"].as_str().expect("session ID").to_string(),
            plugin_id: plugin_id.to_string(),
            events: Vec::new(),
            answered: BTreeSet::new(),
            operations: 0,
            terminal_requests: 0,
        };
        host.await_event("the backend to become ready", |event| {
            event["type"] == "stateChanged" && event["state"] == "ready"
        });
        host
    }

    fn await_event(&mut self, description: &str, matches: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(event) = self.events.iter().find(|event| matches(event)) {
                return event.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for {description}; events: {:#?}", self.events);
            self.drain(remaining);
        }
    }

    fn drain(&mut self, remaining: Duration) {
        let drained = core(
            "lsp.waitEvents",
            json!({ "sessionId": self.session, "timeoutMilliseconds": remaining.as_millis().min(200) as u64 }),
        );
        self.events.extend(drained["events"].as_array().cloned().unwrap_or_default());
    }

    /// The registry after the latest `functionsChanged` event.
    fn functions(&mut self) -> Vec<String> {
        let latest = self
            .events
            .iter()
            .rev()
            .find(|event| event["type"] == "functionsChanged")
            .expect("a functionsChanged event");
        serde_json::from_value(latest["result"]["functions"].clone()).expect("function IDs")
    }

    /// Invokes `function` as the page does, answering the call's credential
    /// requests from `store` until it ends; returns `requestCompleted`.
    fn call(&mut self, store: &mut BTreeMap<String, String>, function: &str, params: Value) -> Value {
        self.operations += 1;
        let operation = format!("{function}-{}", self.operations);
        core(
            "lsp.functionRequest",
            json!({
                "sessionId": self.session,
                "operationId": operation,
                "function": { "pluginID": self.plugin_id, "functionID": function },
                "params": params,
                "context": { "contextID": "docker-e2e", "generation": 1 },
            }),
        );
        let deadline = Instant::now() + WAIT;
        loop {
            let pending: Vec<_> = self
                .events
                .iter()
                .filter(|event| {
                    event["type"] == "hostRequest"
                        && !self.answered.contains(&event["result"]["hostRequestID"].to_string())
                })
                .cloned()
                .collect();
            for request in pending {
                let id = request["result"]["hostRequestID"].clone();
                let params = &request["result"]["params"];
                if request["method"] == "lithe/host/terminal/create" {
                    self.terminal_requests += 1;
                    assert_eq!(params["context"], json!({ "contextID": "docker-e2e", "generation": 1 }));
                    assert_eq!(params["executable"], "docker");
                    let arguments = params["arguments"].as_array().expect("argv");
                    assert_eq!(arguments.len(), 7);
                    assert_eq!(arguments[0], "--host");
                    assert!(arguments[1].as_str().unwrap().starts_with("unix:///"));
                    assert_eq!(&arguments[2..5], &[json!("exec"), json!("--interactive"), json!("--tty")]);
                    assert_eq!(arguments[5].as_str().unwrap().len(), 64);
                    assert_eq!(arguments[6], "/bin/sh");
                    assert_eq!(params["title"], "Docker · e2e");
                    assert_eq!(params["retainedContextBytes"], 16384);
                    assert_eq!(params["environment"]["DOCKER_CONTEXT"], Value::Null);
                    core("lsp.hostResponse", json!({
                        "sessionId": self.session, "hostRequestID": id,
                        "error": { "code": "plugin.host.unavailable", "message": "The Core-only fixture has no native terminal window." }
                    }));
                    self.answered.insert(id.to_string());
                    continue;
                }
                let key = params["key"].as_str().unwrap_or_default().to_string();
                let result = match request["method"].as_str() {
                    Some("lithe/host/browser/openExternal") => {
                        assert_eq!(params["url"], "https://example.com/lithe-docker-e2e");
                        assert_eq!(params["context"]["contextID"], "docker-e2e");
                        json!({ "opened": true, "source": "web" })
                    }
                    Some("lithe/host/credentials/get") => json!({ "secret": store.get(&key) }),
                    Some("lithe/host/credentials/set") => {
                        store.insert(key, params["secret"].as_str().expect("secret").to_string());
                        Value::Null
                    }
                    Some("lithe/host/credentials/delete") => {
                        store.remove(&key);
                        Value::Null
                    }
                    other => panic!("unexpected host request {other:?}"),
                };
                core(
                    "lsp.hostResponse",
                    json!({ "sessionId": self.session, "hostRequestID": id, "result": result }),
                );
                self.answered.insert(id.to_string());
            }
            if let Some(completed) = self
                .events
                .iter()
                .find(|event| event["operationId"] == operation && event["type"] == "requestCompleted")
            {
                if !matches!(
                    function,
                    "containers.list"
                        | "containers.logs"
                        | "containers.stats"
                        | "containers.top"
                        | "contexts.list"
                        | "engine.info"
                        | "events.next"
                        | "images.list"
                        | "images.logo"
                        | "networks.list"
                        | "objects.inspect"
                        | "system.df"
                        | "volumes.list"
                        | "files.list"
                        | "files.uploadBegin"
                        | "files.uploadChunk"
                ) || completed.get("error").is_some()
                {
                    let records = core("diagnostics.drainCoreEvents", json!({}));
                    let event = if completed.get("error").is_some() {
                        "plugins.operation.failed"
                    } else {
                        "plugins.operation.finished"
                    };
                    let terminal: Vec<_> = records["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|record| record["event"] == event && record["op"] == operation)
                        .collect();
                    assert_eq!(
                        terminal.len(),
                        1,
                        "typed terminal for {operation}: {records}"
                    );
                    assert_eq!(
                        terminal[0]["channel"],
                        format!("plugins/{}/host", self.plugin_id)
                    );
                    assert_eq!(
                        terminal[0]["level"],
                        if completed.get("error").is_some() {
                            "ERROR"
                        } else {
                            "INFO"
                        }
                    );
                    assert_eq!(terminal[0]["fields"]["operation"], function);
                }
                return completed.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for {operation}; events: {:#?}", self.events);
            self.drain(remaining);
        }
    }

    /// `call` that must succeed; returns the function's value.
    fn value(&mut self, store: &mut BTreeMap<String, String>, function: &str, params: Value) -> Value {
        let completed = self.call(store, function, params);
        assert!(completed.get("error").is_none(), "{function} failed: {completed}");
        completed["result"]["value"].clone()
    }

    fn stop(&mut self) {
        core("lsp.stopServer", json!({ "sessionId": self.session }));
        self.await_event("the backend to stop", |event| {
            event["type"] == "stateChanged" && event["state"] == "stopped"
        });
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let request = json!({ "id": "docker-e2e-cleanup", "command": "lsp.stopServer", "payload": { "sessionId": self.session } });
        let _ = execute_json(&request.to_string());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let destroy = json!({ "id": "docker-e2e-cleanup", "command": "lsp.destroyServer", "payload": { "sessionId": self.session } });
            let response: Value = serde_json::from_str(&execute_json(&destroy.to_string())).unwrap_or(Value::Null);
            if response["ok"] == true {
                return;
            }
            let wait = json!({ "id": "docker-e2e-cleanup", "command": "lsp.waitEvents", "payload": { "sessionId": self.session, "timeoutMilliseconds": 200 } });
            let _ = execute_json(&wait.to_string());
        }
    }
}
