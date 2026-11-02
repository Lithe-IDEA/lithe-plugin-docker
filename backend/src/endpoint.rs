//! Which engine socket to talk to. Lithe starts the backend from the app,
//! not a shell, so the Docker CLI's own configuration decides, in its order:
//! a choice saved in the plugin, `DOCKER_CONTEXT`, `DOCKER_HOST`, the
//! `currentContext` of `~/.docker/config.json`, then the default socket.
//! When that socket does not exist, the usual sockets of Docker Desktop,
//! OrbStack, Colima, and Rancher Desktop are tried in turn.

use lithe_plugin_sdk::PluginError;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

/// Sockets tried when the configured one is missing, under the home directory.
const KNOWN_SOCKETS: &[&str] = &[
    ".docker/run/docker.sock",
    ".orbstack/run/docker.sock",
    ".colima/default/docker.sock",
    ".colima/docker.sock",
    ".rd/docker.sock",
];

/// What the process environment says about Docker.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    pub home: Option<PathBuf>,
    pub docker_host: Option<String>,
    pub docker_context: Option<String>,
    pub docker_config: Option<PathBuf>,
}

impl Environment {
    pub fn current() -> Self {
        let text = |name: &str| std::env::var(name).ok().filter(|value| !value.trim().is_empty());
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            docker_host: text("DOCKER_HOST"),
            docker_context: text("DOCKER_CONTEXT"),
            docker_config: text("DOCKER_CONFIG").map(PathBuf::from),
        }
    }

    fn config_directory(&self) -> Option<PathBuf> {
        self.docker_config
            .clone()
            .or_else(|| self.home.as_ref().map(|home| home.join(".docker")))
    }
}

/// The choice saved in the plugin's state directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub context: Option<String>,
    pub socket: Option<String>,
}

impl Choice {
    pub fn from_json(value: &Value) -> Self {
        let text = |key: &str| value[key].as_str().filter(|text| !text.is_empty()).map(str::to_string);
        Self {
            context: text("context"),
            socket: text("socket"),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut value = json!({});
        if let Some(context) = &self.context {
            value["context"] = json!(context);
        }
        if let Some(socket) = &self.socket {
            value["socket"] = json!(socket);
        }
        value
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockerContext {
    pub name: String,
    pub host: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub socket: PathBuf,
    pub context: Option<String>,
    /// `setting`, `DOCKER_HOST`, `DOCKER_CONTEXT`, `config`, `default`, or `detected`.
    pub source: &'static str,
}

impl Endpoint {
    pub fn to_json(&self) -> Value {
        json!({ "socket": self.socket, "context": self.context, "source": self.source })
    }
}

/// The CLI's contexts: `default` first, then the others by name.
pub fn contexts(environment: &Environment) -> Vec<DockerContext> {
    let mut found = vec![DockerContext {
        name: "default".into(),
        host: format!("unix://{DEFAULT_SOCKET}"),
        description: "Current DOCKER_HOST based configuration".into(),
    }];
    let Some(meta) = environment
        .config_directory()
        .map(|directory| directory.join("contexts/meta"))
    else {
        return found;
    };
    let mut others: Vec<DockerContext> = fs::read_dir(meta)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let meta: Value = serde_json::from_slice(&fs::read(entry.path().join("meta.json")).ok()?).ok()?;
            Some(DockerContext {
                name: meta["Name"].as_str()?.to_string(),
                host: meta["Endpoints"]["docker"]["Host"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                description: meta["Metadata"]["Description"].as_str().unwrap_or_default().to_string(),
            })
        })
        .filter(|context| context.name != "default")
        .collect();
    others.sort_by(|left, right| left.name.cmp(&right.name));
    found.extend(others);
    found
}

/// The context the CLI would use: `DOCKER_CONTEXT`, else the config's.
pub fn current_context(environment: &Environment) -> Option<String> {
    if environment.docker_context.is_some() {
        return environment.docker_context.clone();
    }
    let config = fs::read(environment.config_directory()?.join("config.json")).ok()?;
    let config: Value = serde_json::from_slice(&config).ok()?;
    config["currentContext"]
        .as_str()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

pub fn resolve(environment: &Environment, choice: &Choice) -> Result<Endpoint, PluginError> {
    if let Some(socket) = &choice.socket {
        return Ok(Endpoint {
            socket: PathBuf::from(socket),
            context: None,
            source: "setting",
        });
    }
    if let Some(name) = &choice.context {
        return context_endpoint(environment, name, "setting");
    }
    // Explicit contexts and hosts must never fall through to another engine:
    // a remove action must stay on the engine the user selected.
    if let Some(name) = &environment.docker_context {
        return context_endpoint(environment, name, "DOCKER_CONTEXT");
    }
    if let Some(host) = &environment.docker_host {
        return Ok(Endpoint {
            socket: unix_socket(host, "DOCKER_HOST")?,
            context: None,
            source: "DOCKER_HOST",
        });
    }
    if let Some(name) = current_context(environment).filter(|name| name != "default") {
        return context_endpoint(environment, &name, "config");
    }
    let configured = Endpoint {
        socket: PathBuf::from(DEFAULT_SOCKET),
        context: Some("default".into()),
        source: "default",
    };
    if exists(&configured.socket) {
        return Ok(configured);
    }
    let detected = environment
        .home
        .iter()
        .flat_map(|home| KNOWN_SOCKETS.iter().map(move |path| home.join(path)))
        .chain([PathBuf::from(DEFAULT_SOCKET)])
        .find(|socket| exists(socket));
    Ok(match detected {
        Some(socket) => Endpoint {
            socket,
            context: None,
            source: "detected",
        },
        None => configured,
    })
}

fn context_endpoint(environment: &Environment, name: &str, source: &'static str) -> Result<Endpoint, PluginError> {
    let context = contexts(environment)
        .into_iter()
        .find(|context| context.name == name)
        .ok_or_else(|| {
            PluginError::new(
                "docker.contextNotFound",
                format!("The Docker context “{name}” doesn't exist."),
            )
        })?;
    Ok(Endpoint {
        socket: unix_socket(&context.host, name)?,
        context: Some(context.name),
        source,
    })
}

fn unix_socket(host: &str, origin: &str) -> Result<PathBuf, PluginError> {
    match host.strip_prefix("unix://") {
        Some(path) if path.starts_with('/') => Ok(PathBuf::from(path)),
        _ => {
            let scheme = host.split("://").next().unwrap_or(host);
            Err(PluginError::new(
                "docker.unsupportedHost",
                format!("{origin} uses a {scheme} endpoint; the Docker plugin connects to local Unix sockets only."),
            ))
        }
    }
}

fn exists(socket: &Path) -> bool {
    fs::metadata(socket).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    struct Home(PathBuf);

    impl Home {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("ldk-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn context(&self, directory: &str, name: &str, host: &str) {
            let meta = self.0.join(".docker/contexts/meta").join(directory);
            fs::create_dir_all(&meta).unwrap();
            let value = json!({ "Name": name, "Metadata": { "Description": "made by a test" }, "Endpoints": { "docker": { "Host": host } } });
            fs::write(meta.join("meta.json"), value.to_string()).unwrap();
        }

        fn environment(&self) -> Environment {
            Environment {
                home: Some(self.0.clone()),
                ..Environment::default()
            }
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn follows_the_current_context_of_the_cli() {
        let home = Home::new("context");
        let socket = home.0.join("orb.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        home.context("0a1b", "orbstack", &format!("unix://{}", socket.display()));
        home.context("ffff", "remote", "ssh://me@example.com");
        fs::write(home.0.join(".docker/config.json"), r#"{"currentContext":"orbstack"}"#).unwrap();
        let environment = home.environment();

        let names: Vec<_> = contexts(&environment).into_iter().map(|context| context.name).collect();
        assert_eq!(names, ["default", "orbstack", "remote"]);
        assert_eq!(current_context(&environment).as_deref(), Some("orbstack"));
        let endpoint = resolve(&environment, &Choice::default()).unwrap();
        assert_eq!(endpoint.socket, socket);
        assert_eq!(endpoint.context.as_deref(), Some("orbstack"));
        assert_eq!(endpoint.source, "config");

        let chosen = Choice {
            context: Some("remote".into()),
            socket: None,
        };
        assert_eq!(
            resolve(&environment, &chosen).unwrap_err().code(),
            "docker.unsupportedHost"
        );
        let missing = Choice {
            context: Some("gone".into()),
            socket: None,
        };
        assert_eq!(
            resolve(&environment, &missing).unwrap_err().code(),
            "docker.contextNotFound"
        );
    }

    #[test]
    fn prefers_docker_host_and_saved_sockets() {
        let home = Home::new("host");
        let socket = home.0.join("engine.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let environment = Environment {
            docker_host: Some(format!("unix://{}", socket.display())),
            ..home.environment()
        };
        let endpoint = resolve(&environment, &Choice::default()).unwrap();
        assert_eq!((endpoint.socket, endpoint.source), (socket.clone(), "DOCKER_HOST"));

        let saved = Choice {
            context: None,
            socket: Some("/tmp/elsewhere.sock".into()),
        };
        let endpoint = resolve(&environment, &saved).unwrap();
        assert_eq!(
            (endpoint.socket, endpoint.source),
            (PathBuf::from("/tmp/elsewhere.sock"), "setting")
        );

        let tcp = Environment {
            docker_host: Some("tcp://127.0.0.1:2375".into()),
            ..home.environment()
        };
        assert_eq!(
            resolve(&tcp, &Choice::default()).unwrap_err().code(),
            "docker.unsupportedHost"
        );
    }

    #[test]
    fn does_not_change_engines_when_an_explicit_socket_is_missing() {
        let home = Home::new("detect");
        fs::create_dir_all(home.0.join(".colima/default")).unwrap();
        let socket = home.0.join(".colima/default/docker.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let environment = Environment {
            docker_host: Some(format!("unix://{}", home.0.join("missing.sock").display())),
            ..home.environment()
        };
        let endpoint = resolve(&environment, &Choice::default()).unwrap();
        assert_eq!(endpoint.socket, home.0.join("missing.sock"));
        assert_eq!(endpoint.source, "DOCKER_HOST");

        home.context("selected", "selected", &format!("unix://{}", socket.display()));
        let environment = Environment {
            docker_context: Some("selected".into()),
            ..environment
        };
        let endpoint = resolve(&environment, &Choice::default()).unwrap();
        assert_eq!((endpoint.socket, endpoint.source), (socket, "DOCKER_CONTEXT"));
    }

    #[test]
    fn saved_choices_round_trip() {
        let choice = Choice {
            context: Some("colima".into()),
            socket: None,
        };
        assert_eq!(Choice::from_json(&choice.to_json()), choice);
        assert_eq!(Choice::from_json(&json!({ "context": "" })), Choice::default());
    }
}
