//! Container files use Docker exec for bounded directory metadata and Docker
//! archives for transfer. Staging is owned by plugin state or the explicitly
//! selected save directory, never by the read-only installed package.

use crate::docker::{self, encode, Docker};
use base64::{engine::general_purpose::STANDARD, Engine};
use lithe_plugin_sdk::{Context, FunctionError, PluginError};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const MAX_FILE: u64 = 256 * 1024 * 1024;
const CHUNK: usize = 192 * 1024;
// User paths are positional arguments, never interpolated into shell source.
const LIST_SCRIPT: &str = r#"[ -d "$1" ] || exit 2
n=0
for entry in "$1"/.[!.]* "$1"/..?* "$1"/*; do
  [ -e "$entry" ] || [ -L "$entry" ] || continue
  n=$((n+1)); [ "$n" -le 5000 ] || exit 3
  printf '%s\000' "${entry##*/}"
  stat -c '%f %s %Y' -- "$entry" || exit 4
done"#;

struct Upload {
    file: fs::File,
    container: String,
    directory: String,
    name: String,
    expected: u64,
    received: u64,
    touched: Instant,
    socket: PathBuf,
}

pub struct Files {
    directory: PathBuf,
    uploads: Mutex<HashMap<String, Upload>>,
    next: AtomicU64,
}

impl Files {
    pub fn new(state: PathBuf) -> Self {
        Self {
            directory: state.join("file-transfers"),
            uploads: Mutex::default(),
            next: AtomicU64::new(1),
        }
    }

    pub fn close(&self) {
        self.uploads.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    pub fn list(docker: &Docker, params: &Value, context: &Context) -> Result<Value, FunctionError> {
        let id = string(params, "id")?;
        let path = container_path(string(params, "path")?)?;
        let created = docker.call(
            context,
            "POST",
            &format!("/containers/{}/exec", encode(id)),
            Some(&json!({
                "AttachStdout": true, "AttachStderr": true, "Tty": false,
                "Cmd": ["/bin/sh", "-c", LIST_SCRIPT, "lithe-files", path],
            })),
        )?;
        let exec = created["Id"]
            .as_str()
            .ok_or_else(|| invalid("Docker returned no exec ID."))?;
        let mut stream = docker.open(
            context,
            "POST",
            &format!("/exec/{}/start", encode(exec)),
            Some(&json!({ "Detach": false, "Tty": false })),
            Some(Duration::from_secs(15)),
        )?;
        docker::check(&mut stream, context)?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut output = Vec::new();
        let mut total_bytes = 0usize;
        loop {
            context.check_cancelled()?;
            if Instant::now() >= deadline {
                return Err(limit().into());
            }
            let mut header = [0; 8];
            match stream.response.body.read(&mut header[..1])? {
                0 => break,
                _ => stream.response.body.read_exact(&mut header[1..])?,
            }
            let size = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
            total_bytes = total_bytes.saturating_add(size);
            if size > 2 * 1024 * 1024 || total_bytes > 2 * 1024 * 1024 {
                return Err(limit().into());
            }
            let mut bytes = vec![0; size];
            stream.response.body.read_exact(&mut bytes)?;
            if header[0] == 1 {
                output.extend(bytes);
            }
        }
        let status = docker.get(context, &format!("/exec/{}/json", encode(exec)))?;
        if status["ExitCode"] != 0 {
            return Err(PluginError::new(
                "docker.filesUnavailable",
                "The container directory could not be listed.",
            )
            .into());
        }
        let mut entries = Vec::new();
        let mut rest = output.as_slice();
        while !rest.is_empty() {
            let end = rest
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(|| invalid("Invalid filename output."))?;
            let name = String::from_utf8_lossy(&rest[..end]).to_string();
            if name.contains('/') || name == "." || name == ".." {
                return Err(invalid("Invalid directory entry.").into());
            }
            rest = &rest[end + 1..];
            let end = rest
                .iter()
                .position(|byte| *byte == b'\n')
                .ok_or_else(|| invalid("Invalid file metadata."))?;
            let metadata = String::from_utf8_lossy(&rest[..end]);
            let fields: Vec<&str> = metadata.split_whitespace().collect();
            if fields.len() != 3 {
                return Err(invalid("Invalid file metadata.").into());
            }
            let mode = u32::from_str_radix(fields[0], 16).map_err(FunctionError::internal)?;
            let kind = match mode & 0xf000 {
                0x4000 => "directory",
                0xa000 => "symlink",
                0x8000 => "file",
                _ => "special",
            };
            entries.push(json!({ "name": name, "kind": kind, "size": fields[1].parse::<u64>().ok(), "modified": fields[2].parse::<i64>().ok() }));
            if entries.len() > 5000 {
                return Err(limit().into());
            }
            rest = &rest[end + 1..];
        }
        entries.sort_by(|a, b| {
            (a["kind"] != "directory", a["name"].as_str()).cmp(&(b["kind"] != "directory", b["name"].as_str()))
        });
        if serde_json::to_vec(&entries)?.len() > 700_000 {
            return Err(limit().into());
        }
        Ok(json!({ "path": path, "entries": entries }))
    }

    pub fn begin(&self, docker: &Docker, params: &Value) -> Result<Value, FunctionError> {
        let container = string(params, "id")?.to_string();
        let directory = container_path(string(params, "path")?)?;
        let name = string(params, "name")?.to_string();
        if name.contains(['/', '\\', '\0']) || name == "." || name == ".." {
            return Err(invalid("Invalid upload filename.").into());
        }
        let expected = params["size"]
            .as_u64()
            .filter(|size| *size <= MAX_FILE)
            .ok_or_else(limit)?;
        let mut uploads = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        uploads.retain(|_, upload| upload.touched.elapsed() < Duration::from_secs(600));
        if uploads.len() >= 2 {
            return Err(limit().into());
        }
        fs::create_dir_all(&self.directory)?;
        // Anonymous temporary files are unlinked immediately on macOS. A
        // crashed or forcibly stopped backend cannot strand uploaded bytes.
        let file = tempfile::tempfile_in(&self.directory)?;
        let token = self.next.fetch_add(1, Ordering::Relaxed).to_string();
        uploads.insert(
            token.clone(),
            Upload {
                file,
                container,
                directory,
                name,
                expected,
                received: 0,
                touched: Instant::now(),
                socket: docker.socket.clone(),
            },
        );
        Ok(json!({ "transfer": token }))
    }

    pub fn chunk(&self, params: &Value) -> Result<Value, FunctionError> {
        let data = string(params, "data")?;
        if data.len() > CHUNK * 2 {
            return Err(limit().into());
        }
        let bytes = STANDARD.decode(data).map_err(|_| invalid("Invalid upload encoding."))?;
        let mut uploads = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        let upload = uploads
            .get_mut(string(params, "transfer")?)
            .ok_or_else(|| invalid("The transfer expired."))?;
        if params["offset"].as_u64() != Some(upload.received)
            || bytes.len() > CHUNK
            || upload.received + bytes.len() as u64 > upload.expected
        {
            return Err(invalid("Invalid upload offset or size.").into());
        }
        upload.file.write_all(&bytes)?;
        upload.received += bytes.len() as u64;
        upload.touched = Instant::now();
        Ok(json!({ "received": upload.received }))
    }

    pub fn abort(&self, params: &Value) -> Result<Value, FunctionError> {
        self.uploads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(string(params, "transfer")?);
        Ok(Value::Null)
    }

    pub fn commit(&self, docker: &Docker, params: &Value, context: &Context) -> Result<Value, FunctionError> {
        let mut upload = self
            .uploads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(string(params, "transfer")?)
            .ok_or_else(|| invalid("The transfer expired."))?;
        if upload.socket != docker.socket {
            return Err(invalid("The Docker connection changed during this transfer.").into());
        }
        if upload.received != upload.expected {
            return Err(invalid("The upload is incomplete.").into());
        }
        context.check_cancelled()?;
        upload.file.seek(SeekFrom::Start(0))?;
        let mut archive = tempfile::tempfile_in(&self.directory)?;
        let mut tar = tar::Builder::new(&mut archive);
        let mut header = tar::Header::new_gnu();
        header.set_size(upload.expected);
        header.set_mode(0o644);
        header.set_mtime(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        header.set_cksum();
        tar.append_data(&mut header, &upload.name, &mut upload.file)?;
        tar.finish()?;
        drop(tar);
        let length = archive.seek(SeekFrom::End(0))?;
        archive.seek(SeekFrom::Start(0))?;
        context.check_cancelled()?;
        context.progress("Uploading file", Some(80));
        let target = format!(
            "/containers/{}/archive?path={}&noOverwriteDirNonDir=1",
            encode(&upload.container),
            encode(&upload.directory)
        );
        docker.upload_archive(context, &target, &mut archive, length)?;
        context.progress("Upload complete", Some(100));
        Ok(Value::Null)
    }

    /// Uses the path chosen in the host's open dialog. The file is streamed
    /// into an owned temporary tar, then uploaded without reading it into JSON.
    pub fn upload_local(&self, docker: &Docker, params: &Value, context: &Context) -> Result<Value, FunctionError> {
        let source = Path::new(string(params, "source")?);
        if !source.is_absolute() || !fs::metadata(source)?.is_file() {
            return Err(invalid("Choose a regular file.").into());
        }
        let mut file = fs::File::open(source)?;
        let metadata = file.metadata()?;
        if metadata.len() > MAX_FILE {
            return Err(limit().into());
        }
        let name = source.file_name().ok_or_else(|| invalid("Invalid filename."))?;
        let path = container_path(string(params, "path")?)?;
        fs::create_dir_all(&self.directory)?;
        let mut archive = tempfile::tempfile_in(&self.directory)?;
        let mut tar = tar::Builder::new(&mut archive);
        let mut header = tar::Header::new_gnu();
        header.set_size(metadata.len());
        header.set_mode(0o644);
        header.set_mtime(
            metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        header.set_cksum();
        context.check_cancelled()?;
        tar.append_data(&mut header, name, &mut file)?;
        tar.finish()?;
        drop(tar);
        let length = archive.seek(SeekFrom::End(0))?;
        archive.seek(SeekFrom::Start(0))?;
        context.check_cancelled()?;
        context.progress("Uploading file", Some(50));
        let target = format!(
            "/containers/{}/archive?path={}&noOverwriteDirNonDir=1",
            encode(string(params, "id")?),
            encode(&path)
        );
        docker.upload_archive(context, &target, &mut archive, length)?;
        context.progress("Upload complete", Some(100));
        Ok(Value::Null)
    }

    /// Copies one regular tar entry to the explicit save destination. Never
    /// extracts archive paths, links or metadata onto the user's filesystem.
    pub fn download(docker: &Docker, params: &Value, context: &Context) -> Result<Value, FunctionError> {
        let id = string(params, "id")?;
        let path = container_path(string(params, "path")?)?;
        let destination = Path::new(string(params, "destination")?);
        if !destination.is_absolute() {
            return Err(invalid("Choose an absolute save destination.").into());
        }
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("Choose a save destination."))?;
        let mut stream = docker.open(
            context,
            "GET",
            &format!("/containers/{}/archive?path={}", encode(id), encode(&path)),
            None,
            Some(Duration::from_secs(30)),
        )?;
        docker::check(&mut stream, context)?;
        let mut archive = tar::Archive::new(stream.response.body.take(MAX_FILE + 1024 * 1024));
        let mut entries = archive.entries()?;
        let mut entry = entries
            .next()
            .transpose()?
            .ok_or_else(|| invalid("The file was not returned."))?;
        if !entry.header().entry_type().is_file() || entry.size() > MAX_FILE {
            return Err(limit().into());
        }
        let expected = entry.size();
        let mut written = 0;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        let mut buffer = [0; 64 * 1024];
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            context.check_cancelled()?;
            if Instant::now() >= deadline {
                return Err(limit().into());
            }
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            file.write_all(&buffer[..count])?;
            written += count as u64;
            context.progress(
                "Downloading file",
                Some((written * 100 / expected.max(1)).min(99) as u8),
            );
        }
        if written != expected {
            return Err(invalid("The download is incomplete.").into());
        }
        file.flush()?;
        context.check_cancelled()?;
        file.persist(destination).map_err(FunctionError::internal)?;
        Ok(json!({ "bytes": written }))
    }
}

fn string<'a>(params: &'a Value, key: &str) -> Result<&'a str, PluginError> {
    params[key]
        .as_str()
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .ok_or_else(|| invalid("Missing file parameter."))
}

fn container_path(path: &str) -> Result<String, PluginError> {
    if !path.starts_with('/') || path.len() > 4096 || path.split('/').any(|part| part == "..") {
        return Err(invalid("Invalid container path."));
    }
    Ok(path.to_string())
}

fn invalid(message: &str) -> PluginError {
    PluginError::new("docker.filesInvalid", message)
}
fn limit() -> PluginError {
    PluginError::new(
        "docker.filesLimit",
        "File transfers support regular files up to 256 MiB and directories up to 5,000 entries.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_reject_invalid_sizes_and_offsets_and_abort_releases_slots() {
        let state = tempfile::tempdir().unwrap();
        let files = Files::new(state.path().into());
        let docker = Docker::new(state.path().join("engine.sock"));
        for (name, size) in [("../outside", 1), ("file", MAX_FILE + 1)] {
            assert!(files
                .begin(
                    &docker,
                    &json!({ "id": "test", "path": "/tmp", "name": name, "size": size })
                )
                .is_err());
        }
        let token = files
            .begin(
                &docker,
                &json!({ "id": "test", "path": "/tmp", "name": "binary.dat", "size": 3 }),
            )
            .unwrap()["transfer"]
            .clone();
        assert!(files
            .chunk(&json!({ "transfer": token, "offset": 1, "data": "AAEC" }))
            .is_err());
        assert_eq!(
            files
                .chunk(&json!({ "transfer": token, "offset": 0, "data": "AAEC" }))
                .unwrap()["received"],
            3
        );
        assert!(files
            .chunk(&json!({ "transfer": token, "offset": 3, "data": "AA==" }))
            .is_err());
        files.abort(&json!({ "transfer": token })).unwrap();
        assert!(files
            .chunk(&json!({ "transfer": token, "offset": 0, "data": "AAEC" }))
            .is_err());
        assert_eq!(fs::read_dir(state.path().join("file-transfers")).unwrap().count(), 0);
    }

    #[test]
    fn container_paths_stay_absolute_and_reject_parent_traversal() {
        for path in ["relative", "/tmp/../etc", "/../root"] {
            assert!(container_path(path).is_err());
        }
        assert_eq!(container_path("/tmp/a space").unwrap(), "/tmp/a space");
    }
}
