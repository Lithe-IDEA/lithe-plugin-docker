//! The backend of the Lithe Docker plugin: the `backend` process Lithe
//! starts when the Docker window first calls a function, with
//! `--state ${stateDir}`. It talks to the Docker Engine API on the engine's
//! Unix socket and keeps only the context or socket the user chose in that
//! directory.

mod docker;
mod endpoint;
mod files;
mod http;
mod logos;
mod service;
mod terminal;
mod time;

use lithe_plugin_sdk::Backend;
use std::path::PathBuf;

fn main() {
    let state = match state_directory() {
        Ok(state) => state,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let mut backend = Backend::new("lithe-docker").version(env!("CARGO_PKG_VERSION"));
    service::Service::new(state, endpoint::Environment::current()).install(&mut backend);
    std::process::exit(backend.listen());
}

fn state_directory() -> Result<PathBuf, String> {
    let mut arguments = std::env::args().skip(1);
    match (arguments.next().as_deref(), arguments.next()) {
        (Some("--state"), Some(path)) if arguments.next().is_none() => Ok(PathBuf::from(path)),
        _ => Err("usage: lithe-docker-backend --state <directory>".into()),
    }
}
