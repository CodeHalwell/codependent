//! Daemon process discovery.
//!
//! Discovery is part of the protocol contract: every client must resolve the
//! same socket path as the daemon, with no coordination other than this code.
//!
//! Data layout (override the root with `CODYPENDENT_DATA_DIR`):
//!
//! ```text
//! ~/.local/share/codypendent/
//! ├── codypendent.db        (daemon-owned; clients never open it)
//! ├── logs/
//! │   └── daemon.log
//! └── artifacts/            (content-addressed store, Phase 1)
//! ```
//!
//! Socket resolution order (Unix sockets are limited to roughly 104–108
//! bytes of path, so the socket cannot always live under the data dir):
//!
//! 1. `CODYPENDENT_SOCKET` — explicit override.
//! 2. `<CODYPENDENT_DATA_DIR>/run/daemon.sock` — when the data dir is
//!    overridden, everything stays under it (test isolation).
//! 3. `$XDG_RUNTIME_DIR/codypendent/daemon.sock` — short, user-private,
//!    cleaned on logout.
//! 4. `<data dir>/run/daemon.sock` — fallback.
//!
//! The pidfile always sits next to the socket.
//!
//! Config layout (override the root with `CODYPENDENT_CONFIG_DIR`):
//!
//! ```text
//! ~/.config/codypendent/
//! └── policy.toml           (global, trusted policy overlay — see policy::)
//! ```
//!
//! `config_dir` resolves independently of `data_dir` (XDG separates config
//! from data), except in the test-isolation case: when `CODYPENDENT_DATA_DIR`
//! is set but `CODYPENDENT_CONFIG_DIR` is not, `config_dir` falls under the
//! overridden data dir too, so an isolated test never reads the real user's
//! config directory.

use std::path::{Path, PathBuf};

/// Conservative bound below the platform SUN_LEN limits (104 on macOS/BSD,
/// 108 on Linux).
pub const MAX_SOCKET_PATH_BYTES: usize = 100;

/// An environment override, or `None` when the variable is unset, empty, or
/// whitespace-only. An empty override is a misconfiguration, not a real path.
fn non_empty_env(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub run_dir: PathBuf,
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    pub log_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("cannot determine a home directory for the current user")]
    NoHomeDirectory,
    #[error(
        "socket path `{path}` is {length} bytes; Unix domain socket paths are limited to \
         roughly 104-108 bytes. Set CODYPENDENT_SOCKET to a shorter path (for example under \
         /tmp) or use a shorter CODYPENDENT_DATA_DIR."
    )]
    SocketPathTooLong { path: String, length: usize },
}

impl RuntimePaths {
    /// Resolve paths from the environment (see module docs for the order).
    pub fn resolve() -> Result<Self, DiscoveryError> {
        // An env var set to an empty (or whitespace-only) string is treated as
        // unset — otherwise `CODYPENDENT_DATA_DIR=""` would make the data dir the
        // empty path and yield relative socket paths like `run/daemon.sock`.
        let data_dir_override = non_empty_env("CODYPENDENT_DATA_DIR");
        let data_dir = match &data_dir_override {
            Some(dir) => dir.clone(),
            None => directories::ProjectDirs::from("", "", "codypendent")
                .ok_or(DiscoveryError::NoHomeDirectory)?
                .data_dir()
                .to_path_buf(),
        };

        let socket_path = if let Some(socket) = non_empty_env("CODYPENDENT_SOCKET") {
            socket
        } else if data_dir_override.is_some() {
            data_dir.join("run").join("daemon.sock")
        } else if let Some(runtime_dir) = non_empty_env("XDG_RUNTIME_DIR") {
            runtime_dir.join("codypendent").join("daemon.sock")
        } else {
            data_dir.join("run").join("daemon.sock")
        };

        // Mirrors the socket-path precedent above: an explicit override wins;
        // otherwise, if the data dir itself was overridden (test isolation),
        // keep config under it too rather than falling through to the real
        // user's config directory; otherwise use the OS config-dir convention.
        let config_dir = if let Some(dir) = non_empty_env("CODYPENDENT_CONFIG_DIR") {
            dir
        } else if data_dir_override.is_some() {
            data_dir.join("config")
        } else {
            directories::ProjectDirs::from("", "", "codypendent")
                .ok_or(DiscoveryError::NoHomeDirectory)?
                .config_dir()
                .to_path_buf()
        };

        let paths = Self::with_socket(data_dir, socket_path, config_dir);
        paths.validate_socket_path()?;
        Ok(paths)
    }

    /// Derive every runtime path from an explicit data directory (tests and
    /// embedded use). The socket lives under `<data_dir>/run/`. `config_dir`
    /// honors `CODYPENDENT_CONFIG_DIR` if set, otherwise defaults under the
    /// given data dir (never the real user's config directory) — the same
    /// test-isolation rule `resolve` applies when the data dir is overridden.
    pub fn from_data_dir(data_dir: PathBuf) -> Self {
        let socket_path = data_dir.join("run").join("daemon.sock");
        let config_dir =
            non_empty_env("CODYPENDENT_CONFIG_DIR").unwrap_or_else(|| data_dir.join("config"));
        Self::with_socket(data_dir, socket_path, config_dir)
    }

    fn with_socket(data_dir: PathBuf, socket_path: PathBuf, config_dir: PathBuf) -> Self {
        let run_dir = socket_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| data_dir.join("run"));
        Self {
            pid_path: run_dir.join("daemon.pid"),
            log_dir: data_dir.join("logs"),
            run_dir,
            socket_path,
            data_dir,
            config_dir,
        }
    }

    /// Location of the global (trusted) policy overlay: `<config_dir>/policy.toml`.
    ///
    /// Only resolves the path — it does not read or validate the file. A
    /// missing file is a normal, unconfigured state; the caller (`PolicyEngine::load`)
    /// decides how to handle that.
    pub fn global_policy_path(&self) -> PathBuf {
        self.config_dir.join("policy.toml")
    }

    /// Location of the operator-declared MCP server list: `<config_dir>/mcp.toml`.
    ///
    /// Only resolves the path — it does not read or validate the file. A
    /// missing file is a normal, unconfigured state; the caller
    /// (`load_mcp_config`) treats it as an empty config.
    pub fn global_mcp_path(&self) -> PathBuf {
        self.config_dir.join("mcp.toml")
    }

    /// Fail early, with an actionable error, instead of letting `bind` fail
    /// with an opaque SUN_LEN error.
    pub fn validate_socket_path(&self) -> Result<(), DiscoveryError> {
        let length = self.socket_path.as_os_str().len();
        if length > MAX_SOCKET_PATH_BYTES {
            return Err(DiscoveryError::SocketPathTooLong {
                path: self.socket_path.display().to_string(),
                length,
            });
        }
        Ok(())
    }

    /// Create the data, run, and log directories. On Unix the directories are
    /// restricted to the owning user (0o700) because the socket grants daemon
    /// access.
    pub fn ensure_directories(&self) -> std::io::Result<()> {
        for dir in [&self.data_dir, &self.run_dir, &self.log_dir] {
            std::fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // All cases below share a single `#[test]` (rather than one each) because
    // every one of them either mutates or depends on the ambient state of
    // `CODYPENDENT_CONFIG_DIR` / `CODYPENDENT_DATA_DIR` / `CODYPENDENT_SOCKET`
    // — process-global env vars that would race across concurrently-run
    // `#[test]` functions. No other test in this crate reads or writes those
    // variables, so this stays fully self-contained.
    #[test]
    fn config_dir_env_override_precedence() {
        std::env::remove_var("CODYPENDENT_SOCKET");
        std::env::remove_var("CODYPENDENT_CONFIG_DIR");
        std::env::remove_var("CODYPENDENT_DATA_DIR");

        // (0) global_policy_path is just config_dir joined with policy.toml;
        // global_mcp_path is the same convention for mcp.toml.
        let paths = RuntimePaths::from_data_dir(PathBuf::from("/tmp/cp-pf3-data"));
        assert_eq!(
            paths.global_policy_path(),
            PathBuf::from("/tmp/cp-pf3-data/config/policy.toml")
        );
        assert_eq!(
            paths.global_mcp_path(),
            PathBuf::from("/tmp/cp-pf3-data/config/mcp.toml")
        );

        // (1) from_data_dir, no override: config_dir must NOT resolve to the
        // real user's config directory (that would leak real policy into an
        // isolated/test RuntimePaths) — it defaults under the given data dir.
        let paths = RuntimePaths::from_data_dir(PathBuf::from("/tmp/cp-pf3-data2"));
        assert_eq!(paths.config_dir, PathBuf::from("/tmp/cp-pf3-data2/config"));

        // (2) from_data_dir honors an explicit override.
        std::env::set_var("CODYPENDENT_CONFIG_DIR", "/tmp/cp-pf3-config-override");
        let paths = RuntimePaths::from_data_dir(PathBuf::from("/tmp/cp-pf3-data3"));
        assert_eq!(
            paths.config_dir,
            PathBuf::from("/tmp/cp-pf3-config-override")
        );
        assert_eq!(
            paths.global_policy_path(),
            PathBuf::from("/tmp/cp-pf3-config-override/policy.toml")
        );
        std::env::remove_var("CODYPENDENT_CONFIG_DIR");

        // (3) resolve(): data dir overridden (test isolation), config dir not
        // — config_dir must stay under the isolated data dir, mirroring the
        // socket-path precedent, never the real user's config directory.
        std::env::set_var("CODYPENDENT_DATA_DIR", "/tmp/cp-pf3-resolve-data");
        let paths = RuntimePaths::resolve().expect("resolves");
        assert_eq!(
            paths.config_dir,
            PathBuf::from("/tmp/cp-pf3-resolve-data/config")
        );
        assert_eq!(
            paths.global_policy_path(),
            PathBuf::from("/tmp/cp-pf3-resolve-data/config/policy.toml")
        );

        // (4) resolve(): an explicit CODYPENDENT_CONFIG_DIR always wins, even
        // with a data dir override in effect.
        std::env::set_var(
            "CODYPENDENT_CONFIG_DIR",
            "/tmp/cp-pf3-resolve-config-override",
        );
        let paths = RuntimePaths::resolve().expect("resolves");
        assert_eq!(
            paths.config_dir,
            PathBuf::from("/tmp/cp-pf3-resolve-config-override")
        );

        std::env::remove_var("CODYPENDENT_DATA_DIR");
        std::env::remove_var("CODYPENDENT_CONFIG_DIR");
    }
}
