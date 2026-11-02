//! Public Docker Hub repository logos. Only Docker Hub image references are
//! queried; credentials and private-registry names never enter these requests.
//! Cache files belong to the host-provided state directory, never the package.

use base64::{engine::general_purpose::STANDARD, Engine};
use lithe_plugin_sdk::{Context, FunctionError, PluginError};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const IMAGE_LIMIT: u64 = 256 * 1024;
const CACHE_VERSION: &str = "hub-logos-v1";

pub struct Logos {
    directory: PathBuf,
}

impl Logos {
    pub fn new(state: PathBuf) -> Self {
        Self {
            directory: state.join(CACHE_VERSION),
        }
    }

    /// Returns a bounded raster data URI or null when Hub has no logo.
    /// Positive results last seven days; missing logos last one hour.
    pub fn get(&self, image: &str, context: &Context) -> Result<Value, FunctionError> {
        context.check_cancelled()?;
        let Some(repository) = hub_repository(image) else {
            return Ok(json!({ "data": null }));
        };
        let (namespace, name) = repository.split_once('/').expect("validated Hub repository");
        let directory = self.directory.join(namespace);
        let path = directory.join(format!("{name}.json"));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Ok(file) = fs::File::open(&path) {
            let mut bytes = Vec::new();
            file.take(IMAGE_LIMIT * 2).read_to_end(&mut bytes)?;
            if let Ok(cached) = serde_json::from_slice::<Value>(&bytes) {
                if cached["expires"].as_u64().is_some_and(|expires| expires > now) {
                    return Ok(json!({ "data": cached["data"] }));
                }
            }
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(4))
            .connect_timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let url = attempt.url();
                let approved = url.scheme() == "https"
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.port_or_known_default() == Some(443)
                    && url
                        .host_str()
                        .is_some_and(|host| host == "hub.docker.com" || host.ends_with(".cloudfront.net"));
                if approved && attempt.previous().len() <= 2 {
                    attempt.follow()
                } else {
                    attempt.error("Unapproved Docker Hub logo redirect")
                }
            }))
            .user_agent(concat!("lithe-docker/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(network_error)?;
        let response = client
            .get(format!(
                "https://hub.docker.com/api/media/repos_logo/v1/{}?type=logo",
                repository.replace('/', "%2F")
            ))
            .send()
            .map_err(network_error)?;
        context.check_cancelled()?;
        let missing = response.status() == reqwest::StatusCode::NOT_FOUND;
        let data = if missing {
            None
        } else {
            let response = response.error_for_status().map_err(network_error)?;
            let mut bytes = Vec::new();
            response.take(IMAGE_LIMIT + 1).read_to_end(&mut bytes)?;
            context.check_cancelled()?;
            // Hub currently serves PNG. Refuse active SVG/HTML content and
            // oversized responses instead of interpreting remote markup.
            if bytes.len() as u64 > IMAGE_LIMIT || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(
                    PluginError::new("docker.logoUnavailable", "Docker Hub returned an unsupported logo.").into(),
                );
            }
            Some(format!("data:image/png;base64,{}", STANDARD.encode(bytes)))
        };
        let result = json!({ "data": data });
        let cache = json!({ "data": data, "expires": now + if missing { 3600 } else { 7 * 86400 } });
        context.check_cancelled()?;
        fs::create_dir_all(&directory)?;
        // Calls for the same image may come from separate windows. Give each
        // writer a unique temporary file before atomically replacing the cache.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = directory.join(format!("{name}-{nonce}.tmp"));
        let stored = fs::write(&temporary, serde_json::to_vec(&cache)?).and_then(|()| fs::rename(&temporary, &path));
        if stored.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        stored?;
        Ok(result)
    }
}

/// Normalizes Docker CLI Hub references, dropping tags/digests. Explicit
/// non-Hub registries and malformed references never leave this machine.
fn hub_repository(image: &str) -> Option<String> {
    if image.starts_with("sha256:") || image.len() > 300 {
        return None;
    }
    let image = image.split('@').next()?;
    let image = image
        .strip_prefix("docker.io/")
        .or_else(|| image.strip_prefix("index.docker.io/"))
        .or_else(|| image.strip_prefix("registry-1.docker.io/"))
        .unwrap_or(image);
    let mut parts: Vec<&str> = image.split('/').collect();
    if parts.len() > 2 || parts.is_empty() {
        return None;
    }
    if parts.len() == 2 && (parts[0].contains(['.', ':']) || parts[0] == "localhost") {
        return None;
    }
    let last = parts.last_mut()?;
    *last = last.split(':').next()?;
    if parts.iter().any(|part| {
        part.is_empty()
            || part.len() > 128
            || !part.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
            || !part.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte))
    }) {
        return None;
    }
    if parts.len() == 1 {
        parts.insert(0, "library");
    }
    Some(parts.join("/"))
}

fn network_error(_: reqwest::Error) -> PluginError {
    PluginError::new("docker.logoUnavailable", "Docker Hub logo is temporarily unavailable.")
}

#[cfg(test)]
mod tests {
    use super::hub_repository;

    #[test]
    fn hub_references_drop_tags_without_disclosing_other_registries() {
        for (image, expected) in [
            ("redis:latest", "library/redis"),
            ("docker.io/library/mysql:8.4", "library/mysql"),
            ("index.docker.io/minio/minio@sha256:abc", "minio/minio"),
            ("registry-1.docker.io/ankane/pgvector:latest", "ankane/pgvector"),
        ] {
            assert_eq!(hub_repository(image).as_deref(), Some(expected));
        }
        for image in [
            "ghcr.io/open-webui/open-webui:main",
            "private.example/team/app",
            "localhost:5000/app",
            "sha256:abcd",
            "../redis",
            "",
            "https://hub.docker.com/redis",
        ] {
            assert_eq!(hub_repository(image), None, "{image}");
        }
    }
}
