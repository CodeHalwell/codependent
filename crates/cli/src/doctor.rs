//! `codypendent doctor` — a read-only health check for the single-binary
//! daemon setup. It never mutates anything; it inspects the binary, the running
//! daemon, the runtime paths, the model configuration, and (best-effort)
//! provider reachability, and prints a checklist. The process exits non-zero
//! when any check FAILS (scriptable), so `doctor` can gate CI or a setup step.
//!
//! The gathering (which does I/O) is kept separate from the pure [`Report`]
//! rendering so the text/JSON output is unit-testable without a daemon.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::BUILD_ID;
use serde::Serialize;

use crate::client;

/// A single check's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Healthy / informational — nothing to do.
    Ok,
    /// Works, but worth attention (offline provider, no daemon yet, stale build).
    Warn,
    /// Broken — `doctor` exits non-zero.
    Fail,
}

impl Status {
    fn mark(self) -> &'static str {
        match self {
            Status::Ok => "✓",
            Status::Warn => "⚠",
            Status::Fail => "✗",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The full checklist. Pure — gatherers push [`Check`]s; renderers read them.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub items: Vec<Check>,
}

impl Report {
    fn push(&mut self, name: &str, status: Status, message: impl Into<String>, hint: Option<&str>) {
        self.items.push(Check {
            name: name.to_string(),
            status,
            message: message.into(),
            hint: hint.map(str::to_string),
        });
    }

    fn ok(&mut self, name: &str, message: impl Into<String>) {
        self.push(name, Status::Ok, message, None);
    }
    fn warn(&mut self, name: &str, message: impl Into<String>, hint: &str) {
        self.push(name, Status::Warn, message, Some(hint));
    }
    fn fail(&mut self, name: &str, message: impl Into<String>, hint: &str) {
        self.push(name, Status::Fail, message, Some(hint));
    }

    /// The worst status across all checks — drives the exit code.
    pub fn worst(&self) -> Status {
        if self.items.iter().any(|c| c.status == Status::Fail) {
            Status::Fail
        } else if self.items.iter().any(|c| c.status == Status::Warn) {
            Status::Warn
        } else {
            Status::Ok
        }
    }

    /// Human-readable checklist (one line per check, indented hint below a
    /// non-ok line). Pure — no I/O, no color (plays nice in pipes and CI logs).
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("codypendent doctor\n\n");
        for c in &self.items {
            out.push_str(&format!(
                "  {} {}: {}\n",
                c.status.mark(),
                c.name,
                c.message
            ));
            if let Some(hint) = &c.hint {
                out.push_str(&format!("      ↳ {hint}\n"));
            }
        }
        let summary = match self.worst() {
            Status::Ok => "all checks passed",
            Status::Warn => "checks passed with warnings",
            Status::Fail => "one or more checks FAILED",
        };
        out.push_str(&format!("\n{summary}\n"));
        out
    }

    /// Machine-readable report: `{ "ok": bool, "checks": [...] }`.
    pub fn render_json(&self) -> String {
        let value = serde_json::json!({
            "ok": self.worst() != Status::Fail,
            "worst": self.worst(),
            "checks": self.items,
        });
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Run all checks and print the report. Returns `true` when nothing FAILED, so
/// `main.rs` can map a fail to a non-zero exit without the library calling
/// `std::process::exit`.
pub async fn run(paths: &RuntimePaths, json: bool, deep: bool) -> anyhow::Result<bool> {
    let mut report = Report::default();
    check_binary(&mut report);
    check_daemon(&mut report, paths).await;
    check_paths(&mut report, paths);
    check_models_and_providers(&mut report, paths, deep).await;

    if json {
        println!("{}", report.render_json());
    } else {
        print!("{}", report.render_text());
    }
    Ok(report.worst() != Status::Fail)
}

fn check_binary(report: &mut Report) {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    report.ok(
        "binary",
        format!(
            "codypendent {} (build {BUILD_ID})\n      {exe}",
            env!("CARGO_PKG_VERSION")
        ),
    );
}

async fn check_daemon(report: &mut Report, paths: &RuntimePaths) {
    if !client::ping(&paths.socket_path).await {
        report.warn(
            "daemon",
            "not running",
            "it starts automatically on first use, or run `codypendent daemon start`",
        );
        return;
    }
    match client::daemon_status(&paths.socket_path).await {
        Ok(status) => {
            let summary = format!(
                "running (pid {}, up {}s, {} active run(s))",
                status.pid, status.uptime_seconds, status.active_run_count
            );
            if status.build_id.is_empty() || status.build_id == BUILD_ID {
                report.ok("daemon", summary);
            } else {
                report.warn(
                    "daemon",
                    format!("{summary} — running a DIFFERENT build ({})", status.build_id),
                    "a newer codypendent is installed; it auto-restarts on next launch (or run `codypendent daemon restart`)",
                );
            }
        }
        Err(error) => report.warn(
            "daemon",
            format!("answered ping but not status: {error:#}"),
            "try `codypendent daemon restart`",
        ),
    }
}

fn check_paths(report: &mut Report, paths: &RuntimePaths) {
    // The data dir is the one hard requirement — it holds the db, socket dir,
    // and models.toml. A non-writable data dir is a genuine failure.
    let data = &paths.data_dir;
    if !data.exists() {
        report.fail(
            "paths",
            format!("data dir does not exist: {}", data.display()),
            "it is created on first run; check CODYPENDENT_DATA_DIR if you set it",
        );
        return;
    }
    let writable = is_writable(data);
    if writable {
        report.ok(
            "paths",
            format!(
                "data {} · config {}",
                data.display(),
                paths.config_dir.display()
            ),
        );
    } else {
        report.fail(
            "paths",
            format!("data dir is not writable: {}", data.display()),
            "fix its permissions (the daemon stores its db, socket, and models here)",
        );
    }
}

/// Best-effort writability probe: try to create (and immediately remove) a
/// uniquely-named temp file in `dir`. Read-only inspection — leaves nothing
/// behind on success.
fn is_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".codypendent-doctor-write-probe");
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

async fn check_models_and_providers(report: &mut Report, paths: &RuntimePaths, deep: bool) {
    let models_path = paths.data_dir.join("models.toml");
    let configs = match codypendent_runtime::models::load_models(&models_path) {
        Err(error) => {
            report.fail(
                "models",
                format!("could not read {}: {error:#}", models_path.display()),
                "create it with at least one [[model]] (see docs); the picker and runs need it",
            );
            return;
        }
        Ok(configs) => configs,
    };
    if configs.is_empty() {
        report.warn(
            "models",
            format!("no models configured in {}", models_path.display()),
            "add a [[model]] entry so runs have something to serve them",
        );
        return;
    }
    report.ok(
        "models",
        format!(
            "{} configured: {}",
            configs.len(),
            configs
                .iter()
                .map(|c| c.id.0.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );

    // Provider reachability — warn-only (offline is legitimate). Local
    // endpoints (Ollama/LM Studio/vLLM on loopback) are always probed with a
    // short TCP connect; hosted endpoints only with `--deep` (a bare TCP
    // connect to host:port, no request, no auth).
    let mut seen = std::collections::BTreeSet::new();
    for config in &configs {
        let base = config.base_url.trim();
        if base.is_empty() || !seen.insert(base.to_string()) {
            continue;
        }
        let local = is_local_url(base);
        if !local && !deep {
            report.ok(
                "provider",
                format!("{base} (hosted — not probed; use `doctor --deep`)"),
            );
            continue;
        }
        let name = if local {
            "provider (local)"
        } else {
            "provider"
        };
        match host_port(base) {
            Some(hostport) => match reachable(&hostport) {
                true => report.ok("provider", format!("{base} — reachable")),
                false => report.warn(
                    name,
                    format!("{base} — not reachable"),
                    "start the local model server, or check the base_url",
                ),
            },
            None => report.warn(
                "provider",
                format!("{base} — could not parse a host:port"),
                "check the base_url in models.toml",
            ),
        }
    }
}

/// Whether a base URL points at the loopback interface (a local model server).
fn is_local_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "::1"]
        .iter()
        .any(|h| lower.contains(h))
}

/// Extract `host:port` from a base URL, defaulting the port from the scheme
/// (https→443, otherwise 80). Deliberately dependency-free string handling —
/// enough for a reachability probe, not a general URL parser.
fn host_port(url: &str) -> Option<String> {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("http", url),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority); // drop userinfo
    if authority.is_empty() {
        return None;
    }
    if authority.contains(':') && !authority.ends_with(']') {
        // host:port (or [ipv6]:port) already present.
        Some(authority.to_string())
    } else {
        let port = if scheme.eq_ignore_ascii_case("https") {
            443
        } else {
            80
        };
        Some(format!("{authority}:{port}"))
    }
}

/// A bounded TCP connect (never hangs): resolve `host:port` and try to open a
/// socket within 2s. True on a successful connect.
fn reachable(hostport: &str) -> bool {
    let Ok(mut addrs) = hostport.to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, status: Status) -> Check {
        Check {
            name: name.to_string(),
            status,
            message: "m".to_string(),
            hint: None,
        }
    }

    #[test]
    fn worst_is_fail_over_warn_over_ok() {
        let mut r = Report::default();
        r.items.push(check("a", Status::Ok));
        assert_eq!(r.worst(), Status::Ok);
        r.items.push(check("b", Status::Warn));
        assert_eq!(r.worst(), Status::Warn);
        r.items.push(check("c", Status::Fail));
        assert_eq!(r.worst(), Status::Fail);
    }

    #[test]
    fn text_render_marks_each_status_and_shows_hints() {
        let mut r = Report::default();
        r.ok("binary", "codypendent 0.1.0");
        r.warn("daemon", "not running", "run `codypendent daemon start`");
        r.fail("models", "missing models.toml", "create it");
        let text = r.render_text();
        assert!(text.contains("✓ binary:"));
        assert!(text.contains("⚠ daemon:"));
        assert!(text.contains("✗ models:"));
        assert!(text.contains("↳ create it"), "a fail hint must render");
        assert!(text.contains("one or more checks FAILED"));
    }

    #[test]
    fn json_render_reports_ok_false_when_a_check_fails() {
        let mut r = Report::default();
        r.ok("binary", "ok");
        r.fail("models", "missing", "create it");
        let json: serde_json::Value = serde_json::from_str(&r.render_json()).expect("valid json");
        assert_eq!(json["ok"], serde_json::json!(false));
        assert_eq!(json["worst"], serde_json::json!("fail"));
        assert_eq!(json["checks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn is_local_url_detects_loopback() {
        assert!(is_local_url("http://localhost:11434/v1"));
        assert!(is_local_url("http://127.0.0.1:1234"));
        assert!(!is_local_url("https://api.openai.com/v1"));
    }

    #[test]
    fn host_port_parses_scheme_host_and_default_ports() {
        assert_eq!(
            host_port("http://localhost:11434/v1").as_deref(),
            Some("localhost:11434")
        );
        assert_eq!(
            host_port("https://api.openai.com/v1").as_deref(),
            Some("api.openai.com:443")
        );
        assert_eq!(
            host_port("http://example.com/path").as_deref(),
            Some("example.com:80")
        );
        assert_eq!(host_port("").as_deref(), None);
    }
}
