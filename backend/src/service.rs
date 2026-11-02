//! The plugin functions the Docker window calls. Each resolves the engine
//! socket afresh, so starting Docker or switching contexts needs no restart,
//! and answers with the fields the page shows rather than the engine's
//! whole objects. Live views long-poll: `events.next` and following
//! `containers.logs` return after their first items or after `wait`, with a
//! cursor the next call resumes from.

use crate::docker::{self, collect, encode, Docker, DEFAULT_TIMEOUT};
use crate::endpoint::{self, Choice, Endpoint, Environment};
use crate::http::{LogLines, Output};
use crate::time;
use lithe_plugin_sdk::{Backend, Context, FunctionError, PluginError, Value};
use serde_json::{json, Map};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A function result is limited to 1 MiB of JSON; log lines stop well below.
const LOG_BUDGET: usize = 600 << 10;
const MAXIMUM_LOG_LINE: usize = 8 << 10;
const CONTAINER_ACTIONS: &[&str] = &["start", "stop", "restart", "pause", "unpause", "kill"];
const COMPOSE_ACTIONS: &[&str] = &["start", "stop", "restart", "remove"];
const PROJECT_LABEL: &str = "com.docker.compose.project";

type Handler = fn(&Service, Value, &Context) -> Result<Value, FunctionError>;

pub struct Service {
    state: PathBuf,
    environment: Environment,
    choice: Mutex<Choice>,
    /// Whether a container has a TTY, which decides how its logs are framed.
    terminals: Mutex<HashMap<(PathBuf, String), bool>>,
    logos: crate::logos::Logos,
    files: crate::files::Files,
}

impl Service {
    pub fn new(state: PathBuf, environment: Environment) -> Arc<Self> {
        let choice = fs::read(state.join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .map(|value| Choice::from_json(&value))
            .unwrap_or_default();
        Arc::new(Self {
            logos: crate::logos::Logos::new(state.clone()),
            files: crate::files::Files::new(state.clone()),
            state,
            environment,
            choice: Mutex::new(choice),
            terminals: Mutex::default(),
        })
    }

    /// Declares every function on `backend`.
    pub fn install(self: &Arc<Self>, backend: &mut Backend) {
        let service = Arc::clone(self);
        backend.on_shutdown(move || {
            service.files.close();
        });
        let functions: &[(&str, Handler)] = &[
            ("browser.open", Self::open_browser),
            ("files.list", Self::files_list),
            ("files.uploadBegin", Self::upload_begin),
            ("files.uploadLocal", Self::upload_local),
            ("files.uploadChunk", Self::upload_chunk),
            ("files.uploadCommit", Self::upload_commit),
            ("files.uploadAbort", Self::upload_abort),
            ("files.download", Self::download_file),
            ("compose.action", Self::compose_action),
            ("containers.action", Self::container_action),
            ("containers.create", Self::create_container),
            ("containers.list", Self::list_containers),
            ("containers.logs", Self::logs),
            ("containers.remove", Self::remove_container),
            ("containers.stats", Self::stats),
            ("containers.top", Self::top),
            ("contexts.list", Self::list_contexts),
            ("contexts.use", Self::use_context),
            ("engine.info", Self::info),
            ("events.next", Self::next_events),
            ("images.list", Self::list_images),
            ("images.logo", Self::image_logo),
            ("images.pull", Self::pull_image),
            ("images.remove", Self::remove_image),
            ("networks.list", Self::list_networks),
            ("networks.remove", Self::remove_network),
            ("objects.inspect", Self::inspect),
            ("system.df", Self::disk_usage),
            ("system.prune", Self::prune),
            ("volumes.list", Self::list_volumes),
            ("volumes.remove", Self::remove_volume),
        ];
        for &(id, handler) in functions {
            let service = Arc::clone(self);
            backend
                .function(id, move |params, context| handler(&service, params, context))
                .expect("valid function ID");
        }
    }

    fn endpoint(&self) -> Result<Endpoint, PluginError> {
        endpoint::resolve(&self.environment, &lock(&self.choice))
    }

    fn image_logo(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        self.logos.get(text(&params, "image")?, context)
    }

    fn open_browser(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let url = text(&params, "url")?;
        let parsed = reqwest::Url::parse(url).map_err(|_| invalid("Invalid web URL."))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(invalid("Only HTTP(S) links without embedded credentials can be opened.").into());
        }
        let scope = context.workspace_context().ok_or_else(|| {
            PluginError::new(
                "docker.browserUnavailable",
                "This host does not provide a browser context.",
            )
        })?;
        if !context.host().supports("lithe/host/browser/openExternal")? {
            return Err(PluginError::new(
                "docker.browserUnavailable",
                "This host does not provide the browser service.",
            )
            .into());
        }
        let mut request = json!({ "url": url });
        request["context"] = scope.to_value();
        Ok(context.host().request("lithe/host/browser/openExternal", request)?)
    }

    fn files_list(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        crate::files::Files::list(&self.docker()?, &params, context)
    }
    fn upload_begin(&self, params: Value, _: &Context) -> Result<Value, FunctionError> {
        self.files.begin(&self.docker()?, &params)
    }
    fn upload_chunk(&self, params: Value, _: &Context) -> Result<Value, FunctionError> {
        self.files.chunk(&params)
    }
    fn upload_abort(&self, params: Value, _: &Context) -> Result<Value, FunctionError> {
        self.files.abort(&params)
    }
    fn upload_local(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        self.files.upload_local(&self.docker()?, &params, context)
    }
    fn upload_commit(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        self.files.commit(&self.docker()?, &params, context)
    }
    fn download_file(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        crate::files::Files::download(&self.docker()?, &params, context)
    }

    fn docker(&self) -> Result<Docker, PluginError> {
        Ok(Docker::new(self.endpoint()?.socket))
    }

    // Engine and contexts.

    fn info(&self, _: Value, context: &Context) -> Result<Value, FunctionError> {
        let endpoint = match self.endpoint() {
            Ok(endpoint) => endpoint,
            Err(error) => return Ok(unreachable(None, &error)),
        };
        let docker = Docker::new(endpoint.socket.clone());
        let version = match docker.get(context, "/version") {
            Ok(version) => version,
            Err(FunctionError::Plugin(error))
                if matches!(
                    error.code(),
                    "docker.unreachable" | "docker.permissionDenied" | "docker.connectionLost"
                ) =>
            {
                return Ok(unreachable(Some(&endpoint), &error));
            }
            Err(error) => return Err(error),
        };
        let info = docker.get(context, "/info")?;
        Ok(json!({
            "reachable": true,
            "endpoint": endpoint.to_json(),
            "version": pick(&version, &["Version", "ApiVersion", "MinAPIVersion", "Os", "Arch", "KernelVersion", "GoVersion"]),
            "platform": version["Platform"]["Name"],
            "info": pick(&info, &[
                "Name", "ServerVersion", "OperatingSystem", "OSType", "Architecture", "NCPU", "MemTotal",
                "Containers", "ContainersRunning", "ContainersPaused", "ContainersStopped", "Images",
                "Driver", "DockerRootDir", "Warnings",
            ]),
        }))
    }

    fn list_contexts(&self, _: Value, _: &Context) -> Result<Value, FunctionError> {
        let contexts: Vec<Value> = endpoint::contexts(&self.environment)
            .into_iter()
            .map(|context| {
                let available = context
                    .host
                    .strip_prefix("unix://")
                    .is_some_and(|path| fs::metadata(path).is_ok());
                json!({ "name": context.name, "host": context.host, "description": context.description, "available": available })
            })
            .collect();
        let endpoint = self.endpoint().ok().map(|endpoint| endpoint.to_json());
        Ok(json!({
            "contexts": contexts,
            "current": endpoint::current_context(&self.environment),
            "choice": lock(&self.choice).to_json(),
            "endpoint": endpoint,
        }))
    }

    /// Saves which context or socket to use; `{}` returns to the CLI's choice.
    fn use_context(&self, params: Value, _: &Context) -> Result<Value, FunctionError> {
        let choice = Choice {
            context: optional_text(&params, "context")?.map(str::to_string),
            socket: optional_text(&params, "socket")?.map(str::to_string),
        };
        if choice.context.is_some() && choice.socket.is_some() {
            return Err(invalid("Choose a context or a socket, not both.").into());
        }
        if choice.socket.as_deref().is_some_and(|socket| !socket.starts_with('/')) {
            return Err(invalid("The socket must be an absolute path.").into());
        }
        let endpoint = endpoint::resolve(&self.environment, &choice)?;
        let mut saved = lock(&self.choice);
        fs::create_dir_all(&self.state)?;
        let temporary = self.state.join("settings.json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&choice.to_json())?)?;
        fs::rename(&temporary, self.state.join("settings.json"))?;
        *saved = choice;
        Ok(endpoint.to_json())
    }

    // Containers.

    fn list_containers(&self, _: Value, context: &Context) -> Result<Value, FunctionError> {
        let docker = self.docker()?;
        let containers = all_containers(&docker, context)?;
        Ok(Value::Array(containers.iter().map(container_summary).collect()))
    }

    fn container_action(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let id = text(&params, "id")?;
        let action = one_of(&params, "action", CONTAINER_ACTIONS)?;
        self.docker()?
            .call(context, "POST", &container_action_target(id, action), None)?;
        Ok(Value::Null)
    }

    fn remove_container(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let id = text(&params, "id")?;
        let target = format!(
            "/containers/{}?force={}&v={}",
            encode(id),
            flag(&params, "force"),
            flag(&params, "volumes")
        );
        self.docker()?.call(context, "DELETE", &target, None)?;
        lock(&self.terminals).retain(|(_, container), _| container != id);
        Ok(Value::Null)
    }

    /// Without `follow`, the last `tail` lines. With it, lines after `since`
    /// (or only new ones), returned once some arrive or `wait` passes.
    fn logs(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let id = text(&params, "id")?;
        let follow = flag(&params, "follow");
        let since = optional_text(&params, "since")?;
        let docker = self.docker()?;
        let multiplexed = !self.has_terminal(&docker, context, id)?;
        let mut target = format!("/containers/{}/logs?stdout=1&stderr=1&timestamps=1", encode(id));
        if follow {
            target.push_str("&follow=1");
            match since {
                Some(since) => target.push_str(&format!("&since={}", encode(since))),
                None => target.push_str("&tail=0"),
            }
        } else {
            target.push_str(&format!("&tail={}", number(&params, "tail", 500, 1, 10_000)));
        }
        let timeout = (!follow).then_some(DEFAULT_TIMEOUT);
        let mut stream = docker.open(context, "GET", &target, None, timeout)?;
        docker::check(&mut stream, context)?;
        let mut lines = LogLines::new(stream.response.body, multiplexed, MAXIMUM_LOG_LINE);
        let (lines, ended, truncated) = if follow {
            let wait = Duration::from_millis(number(&params, "wait", 15_000, 0, 25_000));
            let collected = collect(
                stream.connection,
                move || Ok(lines.next_line()?.map(log_line)),
                log_weight,
                wait,
                Duration::from_millis(150),
                LOG_BUDGET,
            )
            .map_err(|error| docker::io_error(context, error))?;
            context.check_cancelled()?;
            (collected.items, collected.ended, false)
        } else {
            let mut kept = VecDeque::new();
            let mut weighed = 0;
            let mut truncated = false;
            while let Some(line) = lines.next_line().map_err(|error| docker::io_error(context, error))? {
                let line = log_line(line);
                weighed += log_weight(&line);
                kept.push_back(line);
                while weighed > LOG_BUDGET {
                    weighed -= kept.pop_front().as_ref().map_or(0, log_weight);
                    truncated = true;
                }
            }
            (Vec::from(kept), true, truncated)
        };
        let cursor = lines
            .iter()
            .rev()
            .find_map(|line| line["time"].as_str().and_then(time::since_after))
            .or_else(|| since.map(str::to_string));
        Ok(json!({ "lines": lines, "cursor": cursor, "ended": ended, "truncated": truncated }))
    }

    fn has_terminal(&self, docker: &Docker, context: &Context, id: &str) -> Result<bool, FunctionError> {
        let key = (docker.socket.clone(), id.to_string());
        if let Some(&terminal) = lock(&self.terminals).get(&key) {
            return Ok(terminal);
        }
        let inspected = docker.get(context, &format!("/containers/{}/json", encode(id)))?;
        let terminal = inspected["Config"]["Tty"].as_bool().unwrap_or(false);
        lock(&self.terminals).insert(key, terminal);
        Ok(terminal)
    }

    fn stats(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let id = text(&params, "id")?;
        let target = format!("/containers/{}/stats?stream=false", encode(id));
        let stats = self
            .docker()?
            .call_with(context, "GET", &target, None, Duration::from_secs(30))?;
        Ok(stats_summary(&stats))
    }

    fn top(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let id = text(&params, "id")?;
        let top = self
            .docker()?
            .get(context, &format!("/containers/{}/top", encode(id)))?;
        Ok(json!({ "titles": top["Titles"], "processes": top["Processes"] }))
    }

    /// Creates a container from `image`, pulling the image first when the
    /// engine lacks it, and starts it unless `start` is false.
    fn create_container(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let image = text(&params, "image")?;
        let body = container_body(&params, self.environment.home.as_deref())?;
        let mut target = "/containers/create".to_string();
        if let Some(name) = optional_text(&params, "name")? {
            target.push_str(&format!("?name={}", encode(name)));
        }
        let docker = self.docker()?;
        let created = match docker.call(context, "POST", &target, Some(&body)) {
            Err(FunctionError::Plugin(error)) if error.code() == "docker.notFound" => {
                pull(&docker, context, image)?;
                docker.call(context, "POST", &target, Some(&body))?
            }
            other => other?,
        };
        let id = created["Id"].as_str().unwrap_or_default().to_string();
        if params["start"].as_bool() != Some(false) {
            context.progress("Starting the container", None);
            docker.call(context, "POST", &format!("/containers/{}/start", encode(&id)), None)?;
        }
        Ok(json!({ "id": id, "warnings": created["Warnings"] }))
    }

    /// Starts, stops, restarts, or removes every container of a Compose
    /// project, found by its label; removing also removes its networks.
    fn compose_action(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let project = text(&params, "project")?;
        let action = one_of(&params, "action", COMPOSE_ACTIONS)?;
        let docker = self.docker()?;
        let filter = encode(&json!({ "label": [format!("{PROJECT_LABEL}={project}")] }).to_string());
        let containers = docker.get(context, &format!("/containers/json?all=1&filters={filter}"))?;
        let containers = containers.as_array().cloned().unwrap_or_default();
        for (index, container) in containers.iter().enumerate() {
            let id = container["Id"].as_str().unwrap_or_default();
            let name = container_name(container);
            let percentage = (index * 100 / containers.len().max(1)) as u8;
            context.progress(&format!("{} {name}", capital(action)), Some(percentage));
            if action == "remove" {
                docker.call(context, "DELETE", &format!("/containers/{}?force=1", encode(id)), None)?;
                lock(&self.terminals).retain(|(_, container), _| container != id);
            } else {
                docker.call(context, "POST", &container_action_target(id, action), None)?;
            }
        }
        if action == "remove" {
            let networks = docker.get(context, &format!("/networks?filters={filter}"))?;
            for network in networks.as_array().into_iter().flatten() {
                let id = network["Id"].as_str().unwrap_or_default();
                // A network another container still uses stays.
                match docker.call(context, "DELETE", &format!("/networks/{}", encode(id)), None) {
                    Err(FunctionError::Plugin(error)) if error.code() == "docker.conflict" => {}
                    other => {
                        other?;
                    }
                }
            }
        }
        Ok(json!({ "containers": containers.len() }))
    }

    // Images, volumes, and networks.

    fn list_images(&self, _: Value, context: &Context) -> Result<Value, FunctionError> {
        let docker = self.docker()?;
        let images = docker.get(context, "/images/json")?;
        let mut used: HashMap<String, usize> = HashMap::new();
        for container in all_containers(&docker, context)? {
            if let Some(image) = container["ImageID"].as_str() {
                *used.entry(image.to_string()).or_default() += 1;
            }
        }
        let images: Vec<Value> = images
            .as_array()
            .into_iter()
            .flatten()
            .map(|image| {
                let id = image["Id"].as_str().unwrap_or_default();
                let tags: Vec<&str> = image["RepoTags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|tag| *tag != "<none>:<none>")
                    .collect();
                json!({
                    "id": id,
                    "tags": tags,
                    "digests": image["RepoDigests"],
                    "size": image["Size"],
                    "created": image["Created"],
                    "containers": used.get(id).copied().unwrap_or(0),
                    "dangling": tags.is_empty(),
                })
            })
            .collect();
        Ok(Value::Array(images))
    }

    fn pull_image(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let reference = text(&params, "image")?;
        pull(&self.docker()?, context, reference)?;
        Ok(json!({ "image": with_tag(reference) }))
    }

    fn remove_image(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let id = text(&params, "id")?;
        let target = format!("/images/{}?force={}", encode(id), flag(&params, "force"));
        let removed = self.docker()?.call(context, "DELETE", &target, None)?;
        let count = |key: &str| {
            removed
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| entry.get(key).is_some())
                .count()
        };
        Ok(json!({ "untagged": count("Untagged"), "deleted": count("Deleted") }))
    }

    fn list_volumes(&self, _: Value, context: &Context) -> Result<Value, FunctionError> {
        let docker = self.docker()?;
        let volumes = docker.get(context, "/volumes")?;
        let mut users: HashMap<String, Vec<String>> = HashMap::new();
        for container in all_containers(&docker, context)? {
            for mount in container["Mounts"].as_array().into_iter().flatten() {
                if mount["Type"] == "volume" {
                    if let Some(name) = mount["Name"].as_str() {
                        users
                            .entry(name.to_string())
                            .or_default()
                            .push(container_name(&container));
                    }
                }
            }
        }
        let volumes: Vec<Value> = volumes["Volumes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|volume| {
                let name = volume["Name"].as_str().unwrap_or_default();
                let labels = &volume["Labels"];
                json!({
                    "name": name,
                    "driver": volume["Driver"],
                    "mountpoint": volume["Mountpoint"],
                    "created": volume["CreatedAt"],
                    "project": labels[PROJECT_LABEL],
                    "anonymous": labels.get("com.docker.volume.anonymous").is_some()
                        || (name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())),
                    "containers": users.get(name).cloned().unwrap_or_default(),
                })
            })
            .collect();
        Ok(Value::Array(volumes))
    }

    fn remove_volume(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let name = text(&params, "name")?;
        let target = format!("/volumes/{}?force={}", encode(name), flag(&params, "force"));
        self.docker()?.call(context, "DELETE", &target, None)?;
        Ok(Value::Null)
    }

    fn list_networks(&self, _: Value, context: &Context) -> Result<Value, FunctionError> {
        let docker = self.docker()?;
        let networks = docker.get(context, "/networks")?;
        let mut users: HashMap<String, Vec<String>> = HashMap::new();
        for container in all_containers(&docker, context)? {
            for network in container["NetworkSettings"]["Networks"]
                .as_object()
                .into_iter()
                .flatten()
            {
                users
                    .entry(network.0.clone())
                    .or_default()
                    .push(container_name(&container));
            }
        }
        let networks: Vec<Value> = networks
            .as_array()
            .into_iter()
            .flatten()
            .map(|network| {
                let name = network["Name"].as_str().unwrap_or_default();
                let subnets: Vec<&Value> = network["IPAM"]["Config"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|config| &config["Subnet"])
                    .filter(|subnet| subnet.is_string())
                    .collect();
                json!({
                    "id": network["Id"],
                    "name": name,
                    "driver": network["Driver"],
                    "scope": network["Scope"],
                    "internal": network["Internal"],
                    "created": network["Created"],
                    "subnets": subnets,
                    "project": network["Labels"][PROJECT_LABEL],
                    "builtin": matches!(name, "bridge" | "host" | "none"),
                    "containers": users.get(name).cloned().unwrap_or_default(),
                })
            })
            .collect();
        Ok(Value::Array(networks))
    }

    fn remove_network(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let id = text(&params, "id")?;
        self.docker()?
            .call(context, "DELETE", &format!("/networks/{}", encode(id)), None)?;
        Ok(Value::Null)
    }

    /// The engine's whole object, for the page's inspector.
    fn inspect(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let id = encode(text(&params, "id")?);
        let target = match one_of(&params, "kind", &["container", "image", "volume", "network"])? {
            "container" => format!("/containers/{id}/json"),
            "image" => format!("/images/{id}/json"),
            "volume" => format!("/volumes/{id}"),
            _ => format!("/networks/{id}"),
        };
        self.docker()?.get(context, &target)
    }

    // System.

    fn disk_usage(&self, _: Value, context: &Context) -> Result<Value, FunctionError> {
        context.progress("Calculating disk usage", None);
        let usage = self
            .docker()?
            .call_with(context, "GET", "/system/df", None, Duration::from_secs(300))?;
        Ok(disk_usage_summary(&usage))
    }

    fn prune(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let docker = self.docker()?;
        let filter = |key: &str, value: &str| encode(&json!({ key: [value] }).to_string());
        let mut steps: Vec<(&str, &str, String, &str)> = Vec::new();
        if flag(&params, "containers") {
            steps.push((
                "containers",
                "Removing stopped containers",
                "/containers/prune".into(),
                "ContainersDeleted",
            ));
        }
        if flag(&params, "networks") {
            steps.push((
                "networks",
                "Removing unused networks",
                "/networks/prune".into(),
                "NetworksDeleted",
            ));
        }
        if flag(&params, "volumes") {
            let target = if flag(&params, "allVolumes") {
                format!("/volumes/prune?filters={}", filter("all", "true"))
            } else {
                "/volumes/prune".into()
            };
            steps.push(("volumes", "Removing unused volumes", target, "VolumesDeleted"));
        }
        if flag(&params, "images") {
            let dangling = if flag(&params, "allImages") { "false" } else { "true" };
            let target = format!("/images/prune?filters={}", filter("dangling", dangling));
            steps.push(("images", "Removing unused images", target, "ImagesDeleted"));
        }
        if flag(&params, "buildCache") {
            let target = if flag(&params, "allImages") {
                "/build/prune?all=true"
            } else {
                "/build/prune"
            };
            steps.push(("buildCache", "Removing the build cache", target.into(), "CachesDeleted"));
        }
        if steps.is_empty() {
            return Err(invalid("Choose something to remove.").into());
        }
        let mut removed = Map::new();
        let mut reclaimed = 0u64;
        let total = steps.len();
        for (index, (key, message, target, deleted)) in steps.into_iter().enumerate() {
            context.progress(message, Some((index * 100 / total) as u8));
            let answer = docker.call_with(context, "POST", &target, None, Duration::from_secs(600))?;
            removed.insert(key.into(), json!(answer[deleted].as_array().map_or(0, Vec::len)));
            reclaimed += answer["SpaceReclaimed"].as_u64().unwrap_or(0);
        }
        Ok(json!({ "removed": removed, "reclaimed": reclaimed }))
    }

    /// Engine events after `since` (or new ones), returned once some arrive
    /// or `wait` passes. The page refreshes what they touch.
    fn next_events(&self, params: Value, context: &Context) -> Result<Value, FunctionError> {
        let since = optional_text(&params, "since")?;
        let wait = Duration::from_millis(number(&params, "wait", 20_000, 0, 25_000));
        let filter = encode(&json!({ "type": ["container", "image", "volume", "network"] }).to_string());
        let mut target = format!("/events?filters={filter}");
        if let Some(since) = since {
            target.push_str(&format!("&since={}", encode(since)));
        }
        let docker = self.docker()?;
        let mut stream = docker.open(context, "GET", &target, None, None)?;
        docker::check(&mut stream, context)?;
        let mut events = serde_json::Deserializer::from_reader(stream.response.body).into_iter::<Value>();
        let collected = collect(
            stream.connection,
            move || events.next().transpose().map_err(std::io::Error::from),
            |_| 1,
            wait,
            Duration::from_millis(250),
            256,
        )
        .map_err(|error| docker::io_error(context, error))?;
        context.check_cancelled()?;
        let cursor = collected
            .items
            .iter()
            .rev()
            .find_map(|event| event["timeNano"].as_i64())
            .map(time::since_after_nanos)
            .or_else(|| since.map(str::to_string));
        let events: Vec<Value> = collected
            .items
            .iter()
            .map(|event| {
                let action = event["Action"].as_str().unwrap_or_default();
                json!({
                    "type": event["Type"],
                    "action": action.split(':').next().unwrap_or(action),
                    "id": event["Actor"]["ID"],
                    "name": event["Actor"]["Attributes"]["name"],
                    "time": event["timeNano"],
                })
            })
            .collect();
        Ok(json!({ "events": events, "cursor": cursor }))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unreachable(endpoint: Option<&Endpoint>, error: &PluginError) -> Value {
    json!({
        "reachable": false,
        "endpoint": endpoint.map(Endpoint::to_json),
        "error": { "code": error.code(), "message": error.message() },
    })
}

fn all_containers(docker: &Docker, context: &Context) -> Result<Vec<Value>, FunctionError> {
    Ok(docker
        .get(context, "/containers/json?all=1")?
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn container_action_target(id: &str, action: &str) -> String {
    let wait = if matches!(action, "stop" | "restart") {
        "?t=10"
    } else {
        ""
    };
    format!("/containers/{}/{action}{wait}", encode(id))
}

/// Pulls `reference`, reporting the download as one percentage over all
/// layers. Without a tag or digest, `latest` is pulled, as the CLI does,
/// rather than every tag.
fn pull(docker: &Docker, context: &Context, reference: &str) -> Result<(), FunctionError> {
    let reference = with_tag(reference);
    context.progress(&format!("Pulling {reference}"), None);
    let target = format!("/images/create?fromImage={}", encode(&reference));
    let mut stream = docker.open(context, "POST", &target, None, None)?;
    docker::check(&mut stream, context)?;
    let mut layers: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut reported = Instant::now() - Duration::from_secs(1);
    for message in serde_json::Deserializer::from_reader(stream.response.body).into_iter::<Value>() {
        let message = message.map_err(|error| {
            if context.cancellation().is_cancelled() {
                FunctionError::Cancelled
            } else {
                docker::io_error(context, error.into())
            }
        })?;
        if let Some(error) = message["errorDetail"]["message"].as_str().or(message["error"].as_str()) {
            return Err(PluginError::new("docker.pullFailed", error).into());
        }
        let status = message["status"].as_str().unwrap_or_default();
        if let Some(layer) = message["id"].as_str() {
            let detail = &message["progressDetail"];
            match status {
                "Downloading" => {
                    if let (Some(current), Some(total)) = (detail["current"].as_u64(), detail["total"].as_u64()) {
                        layers.insert(layer.to_string(), (current.min(total), total));
                    }
                }
                "Download complete" | "Pull complete" | "Already exists" => {
                    if let Some(progress) = layers.get_mut(layer) {
                        progress.0 = progress.1;
                    }
                }
                _ => {}
            }
        }
        if reported.elapsed() >= Duration::from_millis(200) {
            reported = Instant::now();
            let (current, total) = layers
                .values()
                .fold((0, 0), |sum, layer| (sum.0 + layer.0, sum.1 + layer.1));
            let percentage = (total > 0).then(|| (current * 100 / total) as u8);
            context.progress(&format!("Pulling {reference}: {status}"), percentage);
        }
    }
    context.check_cancelled()?;
    Ok(())
}

pub fn with_tag(reference: &str) -> String {
    let last = reference.rsplit('/').next().unwrap_or(reference);
    if reference.contains('@') || last.contains(':') {
        reference.to_string()
    } else {
        format!("{reference}:latest")
    }
}

fn container_name(container: &Value) -> String {
    container["Names"][0]
        .as_str()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_string()
}

fn container_summary(container: &Value) -> Value {
    let labels = &container["Labels"];
    let mut ports: Vec<(u64, u64, String, String)> = container["Ports"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|port| {
            (
                port["PrivatePort"].as_u64().unwrap_or(0),
                port["PublicPort"].as_u64().unwrap_or(0),
                port["Type"].as_str().unwrap_or("tcp").to_string(),
                port["IP"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    // The engine lists a published port once per address family.
    ports.sort_by(|left, right| {
        (left.0, left.1, &left.2, left.3.contains(':')).cmp(&(right.0, right.1, &right.2, right.3.contains(':')))
    });
    ports.dedup_by(|later, earlier| (later.0, later.1, &later.2) == (earlier.0, earlier.1, &earlier.2));
    let ports: Vec<Value> = ports
        .into_iter()
        .map(|(private, public, protocol, address)| {
            json!({ "private": private, "public": (public > 0).then_some(public), "protocol": protocol, "address": address })
        })
        .collect();
    let networks: Vec<&String> = container["NetworkSettings"]["Networks"]
        .as_object()
        .map(|networks| networks.keys().collect())
        .unwrap_or_default();
    let mounts: Vec<Value> = container["Mounts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|mount| {
            json!({
                "type": mount["Type"],
                "name": mount["Name"],
                "source": mount["Source"],
                "destination": mount["Destination"],
                "readWrite": mount["RW"],
            })
        })
        .collect();
    json!({
        "id": container["Id"],
        "name": container_name(container),
        "image": container["Image"],
        "imageID": container["ImageID"],
        "command": container["Command"],
        "created": container["Created"],
        "state": container["State"],
        "status": container["Status"],
        "ports": ports,
        "networks": networks,
        "mounts": mounts,
        "project": labels[PROJECT_LABEL],
        "service": labels["com.docker.compose.service"],
        "workingDirectory": labels["com.docker.compose.project.working_dir"],
        "configFiles": labels["com.docker.compose.project.config_files"],
    })
}

/// CPU and memory as `docker stats` computes them, from one sample and the
/// engine's previous one.
fn stats_summary(stats: &Value) -> Value {
    let cpu = &stats["cpu_stats"];
    let previous = &stats["precpu_stats"];
    let used = |value: &Value| value.as_u64().unwrap_or(0);
    let cpu_delta = used(&cpu["cpu_usage"]["total_usage"]).saturating_sub(used(&previous["cpu_usage"]["total_usage"]));
    let system_delta = used(&cpu["system_cpu_usage"]).saturating_sub(used(&previous["system_cpu_usage"]));
    let cpus = cpu["online_cpus"]
        .as_u64()
        .filter(|cpus| *cpus > 0)
        .or_else(|| {
            cpu["cpu_usage"]["percpu_usage"]
                .as_array()
                .map(|cpus| cpus.len() as u64)
        })
        .unwrap_or(1)
        .max(1);
    let cpu_percent = if system_delta > 0 {
        cpu_delta as f64 / system_delta as f64 * cpus as f64 * 100.0
    } else {
        0.0
    };
    let memory = &stats["memory_stats"];
    let cache = ["inactive_file", "total_inactive_file"]
        .iter()
        .find_map(|key| memory["stats"][key].as_u64())
        .unwrap_or(0);
    let memory_used = used(&memory["usage"]).saturating_sub(cache);
    let memory_limit = used(&memory["limit"]);
    let (mut received, mut sent) = (0, 0);
    for network in stats["networks"]
        .as_object()
        .into_iter()
        .flat_map(|networks| networks.values())
    {
        received += used(&network["rx_bytes"]);
        sent += used(&network["tx_bytes"]);
    }
    let (mut read, mut written) = (0, 0);
    for entry in stats["blkio_stats"]["io_service_bytes_recursive"]
        .as_array()
        .into_iter()
        .flatten()
    {
        match entry["op"].as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("read") => read += used(&entry["value"]),
            Some("write") => written += used(&entry["value"]),
            _ => {}
        }
    }
    json!({
        "cpuPercent": (cpu_percent * 100.0).round() / 100.0,
        "cpus": cpus,
        "memoryUsed": memory_used,
        "memoryLimit": memory_limit,
        "networkReceived": received,
        "networkSent": sent,
        "blockRead": read,
        "blockWritten": written,
        "processes": stats["pids_stats"]["current"],
    })
}

fn disk_usage_summary(usage: &Value) -> Value {
    let list = |key: &str| usage[key].as_array().cloned().unwrap_or_default();
    let size = |value: &Value| value.as_i64().unwrap_or(0).max(0) as u64;
    let images = list("Images");
    let image_total = usage["LayersSize"]
        .as_u64()
        .unwrap_or_else(|| images.iter().map(|image| size(&image["Size"])).sum());
    let unused_images = images
        .iter()
        .filter(|image| image["Containers"].as_i64().unwrap_or(0) <= 0);
    let containers = list("Containers");
    let volumes = list("Volumes");
    let caches = list("BuildCache");
    json!({
        "images": {
            "count": images.len(),
            "active": images.iter().filter(|image| image["Containers"].as_i64().unwrap_or(0) > 0).count(),
            "size": image_total,
            "reclaimable": unused_images.map(|image| size(&image["Size"]).saturating_sub(size(&image["SharedSize"]))).sum::<u64>(),
        },
        "containers": {
            "count": containers.len(),
            "active": containers.iter().filter(|container| container["State"] == "running").count(),
            "size": containers.iter().map(|container| size(&container["SizeRw"])).sum::<u64>(),
            "reclaimable": containers.iter().filter(|container| container["State"] != "running").map(|container| size(&container["SizeRw"])).sum::<u64>(),
        },
        "volumes": {
            "count": volumes.len(),
            "active": volumes.iter().filter(|volume| volume["UsageData"]["RefCount"].as_i64().unwrap_or(0) > 0).count(),
            "size": volumes.iter().map(|volume| size(&volume["UsageData"]["Size"])).sum::<u64>(),
            "reclaimable": volumes.iter().filter(|volume| volume["UsageData"]["RefCount"].as_i64().unwrap_or(0) <= 0).map(|volume| size(&volume["UsageData"]["Size"])).sum::<u64>(),
        },
        "buildCache": {
            "count": caches.len(),
            "active": caches.iter().filter(|cache| cache["InUse"] == true).count(),
            "size": caches.iter().map(|cache| size(&cache["Size"])).sum::<u64>(),
            "reclaimable": caches.iter().filter(|cache| cache["InUse"] != true && cache["Shared"] != true).map(|cache| size(&cache["Size"])).sum::<u64>(),
        },
    })
}

/// The engine's create body for the page's run form.
fn container_body(params: &Value, home: Option<&std::path::Path>) -> Result<Value, PluginError> {
    let image = text(params, "image")?;
    let mut exposed = Map::new();
    let mut bindings = Map::new();
    for port in params["ports"].as_array().into_iter().flatten() {
        let container = port_number(&port["container"])
            .ok_or_else(|| invalid("A container port must be a number from 1 to 65535."))?;
        let protocol = port["protocol"]
            .as_str()
            .filter(|protocol| !protocol.is_empty())
            .unwrap_or("tcp");
        if !matches!(protocol, "tcp" | "udp" | "sctp") {
            return Err(invalid("A port protocol must be tcp, udp, or sctp."));
        }
        let host = match &port["host"] {
            Value::Null => String::new(),
            Value::String(text) if text.trim().is_empty() => String::new(),
            other => port_number(other)
                .ok_or_else(|| invalid("A host port must be a number from 1 to 65535."))?
                .to_string(),
        };
        let key = format!("{container}/{protocol}");
        exposed.insert(key.clone(), json!({}));
        let binding = json!({ "HostIp": port["address"].as_str().unwrap_or_default(), "HostPort": host });
        bindings
            .entry(key)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("an array")
            .push(binding);
    }
    let mut environment = Vec::new();
    for variable in params["environment"].as_array().into_iter().flatten() {
        let variable = variable.as_str().unwrap_or_default();
        match variable.split_once('=') {
            Some((name, _)) if !name.is_empty() => environment.push(variable.to_string()),
            _ => return Err(invalid("Environment variables are written NAME=value.")),
        }
    }
    let mut binds = Vec::new();
    let mut mounts = Vec::new();
    for volume in params["volumes"].as_array().into_iter().flatten() {
        let source = volume["source"].as_str().unwrap_or_default().trim();
        let target = volume["target"].as_str().unwrap_or_default().trim();
        if source.is_empty() || !target.starts_with('/') {
            return Err(invalid(
                "A volume needs a source and an absolute path in the container.",
            ));
        }
        let read_only = volume["readOnly"].as_bool().unwrap_or(false);
        let source = match (source.strip_prefix("~/"), home) {
            (Some(rest), Some(home)) => home.join(rest).display().to_string(),
            _ => source.to_string(),
        };
        if source.starts_with('/') {
            binds.push(format!("{source}:{target}{}", if read_only { ":ro" } else { "" }));
        } else {
            mounts.push(json!({ "Type": "volume", "Source": source, "Target": target, "ReadOnly": read_only }));
        }
    }
    let restart = params["restart"]
        .as_str()
        .filter(|restart| !restart.is_empty())
        .unwrap_or("no");
    if !matches!(restart, "no" | "always" | "unless-stopped" | "on-failure") {
        return Err(invalid(
            "The restart policy must be no, always, unless-stopped, or on-failure.",
        ));
    }
    let mut host = json!({
        "PortBindings": bindings,
        "Binds": binds,
        "Mounts": mounts,
        "RestartPolicy": { "Name": restart },
        "AutoRemove": params["autoRemove"].as_bool().unwrap_or(false),
    });
    if let Some(network) = params["network"].as_str().filter(|network| !network.is_empty()) {
        host["NetworkMode"] = json!(network);
    }
    let mut body = json!({ "Image": image, "Env": environment, "ExposedPorts": exposed, "HostConfig": host });
    let command: Vec<&str> = params["command"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !command.is_empty() {
        body["Cmd"] = json!(command);
    }
    Ok(body)
}

fn port_number(value: &Value) -> Option<u16> {
    let number = match value {
        Value::Number(number) => number.as_u64()?,
        Value::String(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    u16::try_from(number).ok().filter(|port| *port > 0)
}

fn log_line((output, bytes): (Output, Vec<u8>)) -> Value {
    let line = String::from_utf8_lossy(&bytes);
    let (time, text) = match line.split_once(' ') {
        Some((time, text)) if time.len() >= 20 && time.as_bytes()[0].is_ascii_digit() && time.ends_with('Z') => {
            (time, text)
        }
        _ => ("", &*line),
    };
    let stream = if output == Output::Stderr { "stderr" } else { "stdout" };
    json!({ "stream": stream, "time": time, "text": text })
}

fn log_weight(line: &Value) -> usize {
    // JSON escapes control characters; count the wire representation rather
    // than the unescaped text to stay under the SDK's one MiB result limit.
    serde_json::to_vec(line).map_or(LOG_BUDGET, |bytes| bytes.len() + 1)
}

fn pick(value: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().map(|key| (key.to_string(), value[*key].clone())).collect())
}

fn capital(word: &str) -> String {
    let mut characters = word.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

fn invalid(message: &str) -> PluginError {
    PluginError::new("docker.invalidParams", message)
}

fn text<'a>(params: &'a Value, key: &str) -> Result<&'a str, PluginError> {
    params[key]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| invalid(&format!("“{key}” is required.")))
}

fn optional_text<'a>(params: &'a Value, key: &str) -> Result<Option<&'a str>, PluginError> {
    match &params[key] {
        Value::Null => Ok(None),
        Value::String(text) if text.is_empty() => Ok(None),
        Value::String(text) => Ok(Some(text)),
        _ => Err(invalid(&format!("“{key}” must be text."))),
    }
}

fn one_of<'a>(params: &Value, key: &str, allowed: &[&'a str]) -> Result<&'a str, PluginError> {
    let value = text(params, key)?;
    allowed
        .iter()
        .find(|candidate| **candidate == value)
        .copied()
        .ok_or_else(|| invalid(&format!("“{key}” must be one of {}.", allowed.join(", "))))
}

fn flag(params: &Value, key: &str) -> bool {
    params[key].as_bool().unwrap_or(false)
}

fn number(params: &Value, key: &str, default: u64, minimum: u64, maximum: u64) -> u64 {
    params[key].as_u64().unwrap_or(default).clamp(minimum, maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_latest_unless_a_tag_or_digest_is_given() {
        assert_eq!(with_tag("nginx"), "nginx:latest");
        assert_eq!(with_tag("nginx:1.27"), "nginx:1.27");
        assert_eq!(with_tag("localhost:5000/app"), "localhost:5000/app:latest");
        assert_eq!(with_tag("ghcr.io/me/app@sha256:abc"), "ghcr.io/me/app@sha256:abc");
    }

    #[test]
    fn summarizes_containers_with_one_entry_per_published_port() {
        let summary = container_summary(&json!({
            "Id": "abc", "Names": ["/web-1"], "Image": "nginx", "State": "running", "Status": "Up 2 minutes",
            "Ports": [
                { "IP": "::", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp" },
                { "IP": "0.0.0.0", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp" },
                { "PrivatePort": 443, "Type": "tcp" }
            ],
            "Labels": { "com.docker.compose.project": "shop", "com.docker.compose.service": "web" },
            "NetworkSettings": { "Networks": { "shop_default": {} } },
            "Mounts": []
        }));
        assert_eq!(summary["name"], "web-1");
        assert_eq!(summary["project"], "shop");
        assert_eq!(summary["service"], "web");
        assert_eq!(summary["networks"], json!(["shop_default"]));
        assert_eq!(
            summary["ports"],
            json!([
                { "private": 80, "public": 8080, "protocol": "tcp", "address": "0.0.0.0" },
                { "private": 443, "public": null, "protocol": "tcp", "address": "" }
            ])
        );
    }

    #[test]
    fn computes_stats_like_the_cli() {
        let summary = stats_summary(&json!({
            "cpu_stats": { "cpu_usage": { "total_usage": 300 }, "system_cpu_usage": 2000, "online_cpus": 4 },
            "precpu_stats": { "cpu_usage": { "total_usage": 100 }, "system_cpu_usage": 1000 },
            "memory_stats": { "usage": 1000, "limit": 4000, "stats": { "inactive_file": 200 } },
            "networks": { "eth0": { "rx_bytes": 10, "tx_bytes": 20 }, "eth1": { "rx_bytes": 1, "tx_bytes": 2 } },
            "blkio_stats": { "io_service_bytes_recursive": [{ "op": "Read", "value": 5 }, { "op": "write", "value": 7 }] },
            "pids_stats": { "current": 3 }
        }));
        assert_eq!(summary["cpuPercent"], 80.0);
        assert_eq!(summary["memoryUsed"], 800);
        assert_eq!(summary["memoryLimit"], 4000);
        assert_eq!(
            (summary["networkReceived"].clone(), summary["networkSent"].clone()),
            (json!(11), json!(22))
        );
        assert_eq!(
            (summary["blockRead"].clone(), summary["blockWritten"].clone()),
            (json!(5), json!(7))
        );
        assert_eq!(summary["processes"], 3);
    }

    #[test]
    fn builds_a_create_body_from_the_run_form() {
        let body = container_body(
            &json!({
                "image": "nginx",
                "ports": [{ "host": "8080", "container": 80 }, { "container": "53", "protocol": "udp" }],
                "environment": ["MODE=dev"],
                "volumes": [
                    { "source": "~/site", "target": "/usr/share/nginx/html", "readOnly": true },
                    { "source": "cache", "target": "/var/cache/nginx" }
                ],
                "restart": "unless-stopped",
                "command": ["nginx", "-g", "daemon off;"]
            }),
            Some(std::path::Path::new("/Users/me")),
        )
        .unwrap();
        assert_eq!(body["ExposedPorts"], json!({ "80/tcp": {}, "53/udp": {} }));
        assert_eq!(
            body["HostConfig"]["PortBindings"]["80/tcp"],
            json!([{ "HostIp": "", "HostPort": "8080" }])
        );
        assert_eq!(
            body["HostConfig"]["PortBindings"]["53/udp"],
            json!([{ "HostIp": "", "HostPort": "" }])
        );
        assert_eq!(
            body["HostConfig"]["Binds"],
            json!(["/Users/me/site:/usr/share/nginx/html:ro"])
        );
        assert_eq!(body["HostConfig"]["Mounts"][0]["Source"], "cache");
        assert_eq!(body["HostConfig"]["RestartPolicy"]["Name"], "unless-stopped");
        assert_eq!(body["Env"], json!(["MODE=dev"]));
        assert_eq!(body["Cmd"], json!(["nginx", "-g", "daemon off;"]));

        let bad = container_body(&json!({ "image": "nginx", "ports": [{ "container": 70000 }] }), None);
        assert_eq!(bad.unwrap_err().code(), "docker.invalidParams");
        let bad = container_body(&json!({ "image": "nginx", "environment": ["=x"] }), None);
        assert_eq!(bad.unwrap_err().code(), "docker.invalidParams");
    }

    #[test]
    fn splits_log_timestamps() {
        let line = log_line((
            Output::Stderr,
            b"2024-05-01T12:34:56.123456789Z listening on :80".to_vec(),
        ));
        assert_eq!(
            line,
            json!({ "stream": "stderr", "time": "2024-05-01T12:34:56.123456789Z", "text": "listening on :80" })
        );
        let line = log_line((Output::Stdout, b"no time here".to_vec()));
        assert_eq!(line["time"], "");
        assert_eq!(line["text"], "no time here");
    }

    #[test]
    fn sums_disk_usage() {
        let summary = disk_usage_summary(&json!({
            "LayersSize": 1000,
            "Images": [{ "Size": 600, "SharedSize": 100, "Containers": 0 }, { "Size": 400, "SharedSize": 100, "Containers": 2 }],
            "Containers": [{ "State": "running", "SizeRw": 5 }, { "State": "exited", "SizeRw": 7 }],
            "Volumes": [{ "UsageData": { "Size": 50, "RefCount": 1 } }, { "UsageData": { "Size": 30, "RefCount": 0 } }],
            "BuildCache": [{ "Size": 9, "InUse": false, "Shared": false }, { "Size": 4, "InUse": true }]
        }));
        assert_eq!(
            summary["images"],
            json!({ "count": 2, "active": 1, "size": 1000, "reclaimable": 500 })
        );
        assert_eq!(
            summary["containers"],
            json!({ "count": 2, "active": 1, "size": 12, "reclaimable": 7 })
        );
        assert_eq!(
            summary["volumes"],
            json!({ "count": 2, "active": 1, "size": 80, "reclaimable": 30 })
        );
        assert_eq!(
            summary["buildCache"],
            json!({ "count": 2, "active": 1, "size": 13, "reclaimable": 9 })
        );
    }
}
