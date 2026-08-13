//! The Asaborake job server.
//!
//! Amatsukaze runs as a desktop application with a queue behind it. Asaborake
//! runs on the recording box, where there is no desktop, so the queue is the
//! product: it accepts jobs from `EPGStation` and from the web UI, runs them
//! a few at a time, and reports what it is doing to anyone watching.

// Tests assert; asserting is how they fail. The workspace bans panicking
// constructs in shipping code, not in the suite that checks it.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

pub mod api;
pub mod db;
pub mod disk;
pub mod sources;
pub mod worker;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use asaborake_core::LogoStore;
use asaborake_media::Ffmpeg;
use serde::{Deserialize, Serialize};

pub use db::{Job, JobStatus, NewJob, Store};
pub use worker::{Context, Events, Update};

/// Server configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Address to listen on.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Where the job database lives.
    #[serde(default = "default_database")]
    pub database: PathBuf,
    /// Where learned logos live.
    #[serde(default = "default_logo_dir")]
    pub logo_dir: PathBuf,
    /// How many jobs to run at once.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// What to do when the commercials cannot be found confidently.
    ///
    /// `keep` transcodes the whole recording, which is the safe default.
    /// `block` holds the job instead, so it can be run again once the
    /// channel's logo has been taught rather than quietly producing an uncut
    /// file nobody notices.
    #[serde(default = "default_low_confidence")]
    pub on_low_confidence: asaborake_cmcut::LowConfidencePolicy,
    /// Directories the logo tool may read recordings from.
    ///
    /// The engine serves frames out of these so a browser can show what a
    /// recording looks like. Nothing outside them is readable, because a path
    /// arriving over HTTP is not to be trusted with the filesystem. Empty by
    /// default, which disables frame serving entirely.
    #[serde(default)]
    pub recording_dirs: Vec<PathBuf>,
    /// Path to ffmpeg, when it is not on `PATH`.
    #[serde(default)]
    pub ffmpeg: Option<PathBuf>,
    /// Path to ffprobe, when it is not on `PATH`.
    #[serde(default)]
    pub ffprobe: Option<PathBuf>,
}

const fn default_low_confidence() -> asaborake_cmcut::LowConfidencePolicy {
    asaborake_cmcut::LowConfidencePolicy::Keep
}
fn default_listen() -> String {
    // Loopback by default. The engine has no authentication of its own; the
    // web app is what faces the network, and it proxies to this.
    "127.0.0.1:8081".to_owned()
}
fn default_database() -> PathBuf {
    PathBuf::from("/var/lib/asaborake/jobs.db")
}
fn default_logo_dir() -> PathBuf {
    PathBuf::from("/var/lib/asaborake/logos")
}
const fn default_concurrency() -> usize {
    // One job saturates a GPU encoder and most of a CPU during analysis.
    // Two lets a short job past a long one without thrashing.
    2
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            database: default_database(),
            logo_dir: default_logo_dir(),
            concurrency: default_concurrency(),
            on_low_confidence: default_low_confidence(),
            recording_dirs: Vec::new(),
            ffmpeg: None,
            ffprobe: None,
        }
    }
}

impl Config {
    /// Read configuration from a TOML file.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the file cannot be read, or [`Error::Config`]
    /// if it cannot be parsed.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| Error::Config {
            source: Box::new(source),
        })
    }
}

/// Run the server until the process is asked to stop.
///
/// # Errors
/// Returns an error if ffmpeg is missing, the database cannot be opened, or
/// the listen address cannot be bound.
pub async fn serve(config: Config) -> Result<(), Error> {
    let ffmpeg = Ffmpeg::discover(config.ffmpeg.as_deref(), config.ffprobe.as_deref())
        .map_err(Error::Media)?;

    if let Some(parent) = config.database.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let store = Store::open(&config.database).await?;

    // Anything left marked running was interrupted by a restart; nothing else
    // could have left it that way.
    match store.requeue_interrupted().await? {
        0 => {}
        count => tracing::warn!(count, "requeued jobs interrupted by a restart"),
    }

    let logos = match LogoStore::open(&config.logo_dir) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            // A missing logo store costs three extra decoding passes per job
            // and nothing else, so it must not stop the server starting.
            tracing::warn!(%error, "logo store unavailable; logos will be relearned each time");
            None
        }
    };

    let context = Context {
        store,
        events: Events::new(),
        ffmpeg: Arc::new(ffmpeg),
        logos,
        config: Arc::new(config.clone()),
        wake: Arc::new(tokio::sync::Notify::new()),
    };

    let workers = worker::spawn_pool(&context, config.concurrency);
    tracing::info!(
        workers = workers.len(),
        listen = %config.listen,
        "Asaborake server started"
    );

    let app = api::router(context).layer(
        tower_http::cors::CorsLayer::new()
            .allow_origin(tower_http::cors::Any)
            .allow_methods(tower_http::cors::Any)
            .allow_headers(tower_http::cors::Any),
    );

    let address: SocketAddr = config
        .listen
        .parse()
        .map_err(|_| Error::BadListenAddress(config.listen.clone()))?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|source| Error::Io {
            path: PathBuf::from(&config.listen),
            source,
        })?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|source| Error::Io {
            path: PathBuf::from("http server"),
            source,
        })?;

    Ok(())
}

/// Resolve when the process is asked to stop.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

/// Errors from the server.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The database failed.
    #[error("database error")]
    Database(#[source] sqlx::Error),

    /// The schema could not be brought up to date.
    #[error("failed to migrate the job database")]
    Migrate(#[source] Box<sqlx::migrate::MigrateError>),

    /// ffmpeg is missing or unusable.
    #[error("ffmpeg is unavailable")]
    Media(#[source] asaborake_media::Error),

    /// The configuration file could not be parsed.
    #[error("cannot parse configuration")]
    Config {
        /// The underlying failure.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// The listen address is not a socket address.
    #[error("'{0}' is not a valid listen address, expected something like 127.0.0.1:8081")]
    BadListenAddress(String),

    /// A file or socket failed.
    #[error("i/o error on {path}")]
    Io {
        /// What was being used.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_configuration_listens_on_loopback() {
        // The engine has no authentication; exposing it directly would be a
        // hole, so the default keeps it local and the web app faces outward.
        let config = Config::default();
        assert!(config.listen.starts_with("127.0.0.1"), "{}", config.listen);
        assert!(config.concurrency >= 1);
    }

    #[test]
    fn configuration_fills_in_everything_that_was_omitted() {
        let config: Config = toml::from_str("listen = \"0.0.0.0:9000\"").expect("parses");
        assert_eq!(config.listen, "0.0.0.0:9000");
        assert_eq!(config.concurrency, default_concurrency());
        assert_eq!(config.database, default_database());
    }

    #[test]
    fn a_malformed_configuration_is_rejected() {
        assert!(toml::from_str::<Config>("concurrency = \"lots\"").is_err());
    }
}
