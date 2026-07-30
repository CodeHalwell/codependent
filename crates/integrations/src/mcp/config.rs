//! Loader for `<config_dir>/codypendent/mcp.toml` — the operator-declared list
//! of external MCP (Model Context Protocol) servers the agent may consume tools
//! from.
//!
//! Follows the `models.toml` / trusted-publisher-store convention: a bare
//! `[[server]]` TOML array, a **missing file is an empty config** (no servers —
//! fine), and a malformed file is a hard error FOR THE FEATURE (the daemon logs
//! it loudly and boots with no MCP servers; `codypendent mcp list` fails
//! non-zero). The loader takes an explicit path; resolving `<config_dir>` is
//! the caller's job. A server absent from this file is unreachable — the model
//! can never name one into existence.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `inherit_environment` defaults ON: the launch line is operator-declared
/// trusted (unlike `ExecuteCommand`'s model-controlled env, which defaults
/// empty). `inherit_environment = false` plus explicit `env` pairs gives a
/// hermetic launch.
fn default_inherit_environment() -> bool {
    true
}

/// One `[[server]]` entry: how to launch one MCP stdio server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpServerConfig {
    /// Server id — used in tool names (`mcp.<server>.<tool>`), policy keys, and
    /// logs. Must be non-empty and unique within the file.
    pub name: String,
    /// The executable to spawn (e.g. `npx`).
    pub command: String,
    /// Arguments to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Explicit environment pairs (`env = [["KEY", "value"]]`), merged over the
    /// inherited environment. Secrets enter the child through here by the
    /// operator's declaration; this crate never resolves or stores them
    /// anywhere else.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Whether the child inherits the daemon's environment (default true — see
    /// [`default_inherit_environment`]).
    #[serde(default = "default_inherit_environment")]
    pub inherit_environment: bool,
}

/// The parsed `mcp.toml`: zero or more declared servers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpConfig {
    /// The declared servers, in file order.
    pub servers: Vec<McpServerConfig>,
}

/// The on-disk shape: a bare array of `[[server]]` tables (the `models.toml`
/// layout convention).
#[derive(Debug, Deserialize)]
struct McpFile {
    #[serde(default, rename = "server")]
    servers: Vec<McpServerConfig>,
}

/// A failure loading `mcp.toml`.
#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    /// The file could not be read (other than not existing — a missing file is
    /// an empty config, not an error).
    #[error("failed to read mcp config file at {path}: {source}")]
    Read {
        /// The config path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid TOML / does not match the `[[server]]` shape.
    #[error("failed to parse mcp config file at {path}: {source}")]
    Parse {
        /// The config path.
        path: PathBuf,
        /// The underlying parse error.
        #[source]
        source: toml::de::Error,
    },
    /// The file parsed but failed validation (empty/duplicate names, empty
    /// command).
    #[error("invalid mcp config file at {path}: {reason}")]
    Invalid {
        /// The config path.
        path: PathBuf,
        /// Why the file was rejected.
        reason: String,
    },
}

/// Load `mcp.toml` at `path`. A **missing file is an empty config** (`Ok`), not
/// an error. Names become tool-name prefixes and policy keys, so empty and
/// duplicate server names — and empty commands — are rejected here with a
/// legible error rather than colliding downstream.
pub fn load_mcp_config(path: &Path) -> Result<McpConfig, McpConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(McpConfig::default());
        }
        Err(source) => {
            return Err(McpConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let file: McpFile = toml::from_str(&text).map_err(|source| McpConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    validate(path, &file.servers)?;
    Ok(McpConfig {
        servers: file.servers,
    })
}

/// Reject configs whose servers could not be told apart or launched: empty or
/// duplicate names (they are tool-name prefixes and policy keys) and empty
/// commands.
fn validate(path: &Path, servers: &[McpServerConfig]) -> Result<(), McpConfigError> {
    for (index, server) in servers.iter().enumerate() {
        let invalid = |reason: String| McpConfigError::Invalid {
            path: path.to_path_buf(),
            reason,
        };
        if server.name.trim().is_empty() {
            return Err(invalid(format!(
                "server at index {index} has an empty name (names become tool-name prefixes and policy keys)"
            )));
        }
        if servers[..index]
            .iter()
            .any(|other| other.name == server.name)
        {
            return Err(invalid(format!("duplicate server name `{}`", server.name)));
        }
        if server.command.trim().is_empty() {
            return Err(invalid(format!(
                "server `{}` has an empty command",
                server.name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(contents: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), contents).expect("write temp file");
        file
    }

    #[test]
    fn a_missing_file_is_an_empty_config() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = load_mcp_config(&dir.path().join("mcp.toml")).expect("missing file loads");
        assert!(config.servers.is_empty());
    }

    #[test]
    fn parses_servers_with_env_pairs_and_inherit_defaults() {
        let file = write_temp(
            r#"
[[server]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = [["GITHUB_TOKEN", "secret"]]

[[server]]
name = "hermetic"
command = "/usr/local/bin/mcp-fs"
inherit_environment = false
"#,
        );
        let config = load_mcp_config(file.path()).expect("parses");
        assert_eq!(config.servers.len(), 2);

        let github = &config.servers[0];
        assert_eq!(github.name, "github");
        assert_eq!(github.command, "npx");
        assert_eq!(
            github.args,
            vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-github".to_string()
            ]
        );
        assert_eq!(
            github.env,
            vec![("GITHUB_TOKEN".to_string(), "secret".to_string())]
        );
        assert!(
            github.inherit_environment,
            "inherit_environment defaults to true"
        );

        let hermetic = &config.servers[1];
        assert!(!hermetic.inherit_environment);
        assert!(hermetic.args.is_empty());
        assert!(hermetic.env.is_empty());
    }

    #[test]
    fn malformed_toml_is_a_hard_error() {
        let file = write_temp("[[server]\nname = ");
        let error = load_mcp_config(file.path()).expect_err("malformed TOML fails");
        assert!(matches!(error, McpConfigError::Parse { .. }));
    }

    #[test]
    fn duplicate_server_names_are_rejected() {
        let file = write_temp(
            r#"
[[server]]
name = "github"
command = "a"

[[server]]
name = "github"
command = "b"
"#,
        );
        let error = load_mcp_config(file.path()).expect_err("duplicates fail");
        let message = error.to_string();
        assert!(matches!(error, McpConfigError::Invalid { .. }));
        assert!(message.contains("duplicate"), "got: {message}");
        assert!(message.contains("github"), "got: {message}");
    }

    #[test]
    fn an_empty_server_name_is_rejected() {
        let file = write_temp("[[server]]\nname = \"\"\ncommand = \"a\"\n");
        let error = load_mcp_config(file.path()).expect_err("empty name fails");
        assert!(matches!(error, McpConfigError::Invalid { .. }));
        assert!(error.to_string().contains("empty name"));
    }

    #[test]
    fn an_empty_command_is_rejected() {
        let file = write_temp("[[server]]\nname = \"x\"\ncommand = \"\"\n");
        let error = load_mcp_config(file.path()).expect_err("empty command fails");
        let message = error.to_string();
        assert!(matches!(error, McpConfigError::Invalid { .. }));
        assert!(message.contains("empty command"), "got: {message}");
        assert!(message.contains('x'), "got: {message}");
    }
}
