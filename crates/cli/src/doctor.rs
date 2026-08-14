//! `codypendent doctor` — a read-only health check for the single-binary
//! daemon setup. It never mutates anything; it inspects the binary, the running
//! daemon, the runtime paths, the model configuration, and (best-effort)
//! provider/model readiness, and prints a checklist. The process exits non-zero
//! when any check FAILS (scriptable), so `doctor` can gate CI or a setup step.
//!
//! The gathering (which does I/O) is kept separate from the pure [`Report`]
//! rendering so the text/JSON output is unit-testable without a daemon.

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
    check_code_graph(&mut report, paths).await;
    check_voice(&mut report, paths);

    if json {
        println!("{}", report.render_json());
    } else {
        print!("{}", report.render_text());
    }
    Ok(report.worst() != Status::Fail)
}

fn check_binary(report: &mut Report) {
    let resolved = std::env::current_exe();
    let exe = resolved
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    report.ok(
        "binary",
        format!(
            "codypendent {} (build {BUILD_ID})\n      {exe}",
            env!("CARGO_PKG_VERSION")
        ),
    );
    let launcher = resolved.ok().and_then(|binary| {
        binary
            .parent()
            .map(|directory| directory.join("codypendent-ui-worker-launcher"))
    });
    if launcher
        .as_ref()
        .is_some_and(|path| trusted_ui_launcher(path))
    {
        report.ok(
            "UI worker launcher",
            launcher.expect("checked launcher").display().to_string(),
        );
    } else {
        report.fail(
            "UI worker launcher",
            "missing beside codypendent; embedded plugin UIs will fail closed",
            "reinstall the complete release bundle",
        );
    }
}

fn trusted_ui_launcher(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        path.metadata().is_ok_and(|metadata| {
            metadata.is_file()
                && metadata.permissions().mode() & 0o111 != 0
                && metadata.permissions().mode() & 0o022 == 0
        })
    }
    #[cfg(not(unix))]
    path.is_file()
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
    let auth = match codypendent_runtime::auth::AuthStore::load(&paths.data_dir) {
        Ok(auth) => auth,
        Err(error) => {
            report.warn(
                "model credentials",
                format!("could not read auth.json: {error}"),
                "repair or remove the corrupt auth.json before relying on stored keys",
            );
            codypendent_runtime::auth::AuthStore::default()
        }
    };
    let registry = codypendent_runtime::models::ModelRegistry::new(configs.clone()).with_auth(auth);
    let acp_store = codypendent_integrations::acp_registry::AcpRegistryStore::new(&paths.data_dir);
    let mut checked = 0usize;
    let mut healthy = 0usize;
    for config in &configs {
        let base = config.base_url.trim();
        let local = is_local_url(base);
        if !local && !deep && !base.is_empty() {
            report.warn(
                "model",
                format!("{} ({}) — not verified", config.id, config.model),
                "run `codypendent doctor --deep` to verify hosted credentials and model availability",
            );
            continue;
        }
        checked += 1;
        let readiness = if config.provider == "acp" {
            acp_store
                .launch_spec(&config.model)
                .map(|_| ())
                .map_err(|error| error.to_string())
        } else {
            registry
                .check_model(&config.id)
                .await
                .map_err(|error| error.to_string())
        };
        match readiness {
            Ok(()) => {
                healthy += 1;
                report.ok("model", format!("{} ({}) — ready", config.id, config.model));
            }
            Err(error) => report.warn(
                "model",
                format!("{} ({}) — {error}", config.id, config.model),
                "install/fix this model or move a healthy model ahead of it in the policy",
            ),
        }
    }

    if healthy > 0 {
        report.ok(
            "models",
            format!(
                "{healthy} of {} configured model(s) verified ready",
                configs.len()
            ),
        );
    } else if checked == configs.len() {
        report.fail(
            "models",
            format!(
                "none of the {} configured model(s) is usable",
                configs.len()
            ),
            "open the provider catalog, install/select a listed model, then run doctor again",
        );
    } else {
        report.warn(
            "models",
            format!("{} configured; none verified without --deep", configs.len()),
            "run `codypendent doctor --deep` before starting a hosted run",
        );
    }
}

/// Voice readiness (outcome 8 / voice v1 rubric 8, 2026-08-13 review F6):
/// whether push-to-talk input and spoken replies are configured and
/// reachable. Before this, `doctor` had ZERO voice checks despite three of
/// this feature's own failure modes — a destroyed `[transcription]`/`[speech]`
/// table, a key that resolves nowhere, a missing recorder/player — having no
/// other diagnostic anywhere in the product; a user whose voice stopped
/// working had no supported way to find out why short of reading
/// Whether this checkout has a code graph at all, and how big it is.
///
/// `doctor` is the command a user reaches for when something the agent depends
/// on "isn't being built", and the code graph was invisible to it: the graph was
/// folded only as a side effect of opening a session or starting a run, and
/// nothing anywhere reported whether that had happened. An empty graph is a
/// WARN, never a FAIL — a fresh checkout legitimately has none, and a repository
/// in a language no grammar covers legitimately never will — but it always names
/// the command that builds it, which is the step that was missing.
///
/// Read-only and daemon-free, like the rest of `doctor`: it opens the daemon's
/// database only if the file already exists, and never creates one.
async fn check_code_graph(report: &mut Report, paths: &RuntimePaths) {
    let database_path = paths.data_dir.join("codypendent.db");
    if !database_path.exists() {
        report.ok(
            "code graph",
            "no database yet — the graph is built on first use, or on `codypendent graph build`",
        );
        return;
    }
    let Ok(dir) = std::env::current_dir() else {
        report.warn(
            "code graph",
            "could not resolve the current directory, so no repository to check",
            "run `codypendent doctor` from inside a checkout",
        );
        return;
    };
    // The checkout, never the directory as-opened — the daemon stores nodes
    // under the Git toplevel, so hashing a subdirectory would report "empty"
    // for a graph that is in fact populated. `crate::repo_anchor` is the one
    // accessor for that resolution (the same trap that emptied the document
    // list in the 2026-08-13 review).
    let repository = crate::repo_anchor::anchor_repository_id(&dir);
    let pool = match codypendent_knowledge::db::open(&database_path).await {
        Ok(pool) => pool,
        Err(error) => {
            report.warn(
                "code graph",
                format!("could not open {}: {error}", database_path.display()),
                "check the daemon's data directory is readable",
            );
            return;
        }
    };
    let counted: Result<(i64, i64), sqlx::Error> = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT source_path) FROM code_nodes WHERE repository = ?",
    )
    .bind(repository.to_string())
    .fetch_one(&pool)
    .await;
    match counted {
        Ok((0, _)) => report.warn(
            "code graph",
            format!(
                "empty for {} — the agent has no symbol map for it",
                dir.display()
            ),
            "run `codypendent graph build`: it folds the graph and reports which files were \
             walked and which extensions produced nothing. (`codypendent index rebuild` \
             rebuilds the SEARCH indexes and does NOT touch the code graph.)",
        ),
        Ok((nodes, files)) => report.ok(
            "code graph",
            format!(
                "{nodes} node(s) across {files} file(s); `codypendent graph status` for detail"
            ),
        ),
        Err(error) => report.warn(
            "code graph",
            format!("could not read the graph: {error}"),
            "run `codypendent graph status` for the full error",
        ),
    }
}

/// `models.toml` by hand.
fn check_voice(report: &mut Report, paths: &RuntimePaths) {
    let models_path = paths.data_dir.join("models.toml");
    let audio = match codypendent_runtime::models::load_audio_models(&models_path) {
        Ok(audio) => audio,
        Err(error) => {
            report.fail(
                "voice",
                format!(
                    "could not parse the voice tables in {}: {error:#}",
                    models_path.display()
                ),
                "fix the [transcription]/[speech] syntax in models.toml — a typo silently \
                 disables voice rather than failing loudly anywhere else",
            );
            return;
        }
    };
    if audio.transcription.is_none() && audio.speech.is_none() {
        report.ok(
            "voice",
            "not configured (no [transcription]/[speech] table) — push-to-talk input and \
             spoken replies are both off",
        );
        return;
    }

    let auth = codypendent_runtime::auth::AuthStore::load(&paths.data_dir).unwrap_or_default();
    if let Some(stt) = &audio.transcription {
        check_voice_endpoint(
            report,
            "voice input (STT)",
            stt,
            &auth,
            "transcription",
            "the DAEMON's environment (not the shell `doctor` runs in) must export it before \
             the daemon starts",
        );
    }
    if let Some(tts) = &audio.speech {
        check_voice_endpoint(
            report,
            "voice output (TTS)",
            tts,
            &auth,
            "speech",
            "the TUI's environment (not the shell `doctor` runs in) must export it before the \
             TUI starts",
        );
    }

    // Recorder/player readiness reuses the SAME selection logic the TUI itself
    // runs at startup (`crate::voice`), so `doctor` can never disagree with
    // what pressing the push-to-talk key will actually do.
    let voice_config = crate::voice::load_voice_config(&models_path);
    if audio.transcription.is_some() {
        let path_var = std::env::var("PATH").ok();
        match crate::voice::select_recorder(&voice_config, path_var.as_deref()) {
            Some(recorder) => report.ok(
                "voice recorder",
                format!("{:?} ready for push-to-talk", recorder.source),
            ),
            None => report.warn(
                "voice recorder",
                "no recorder found on $PATH and no voice.record_command set",
                "install sox (`rec`), alsa-utils (`arecord`), or ffmpeg, or set \
                 voice.record_command in models.toml",
            ),
        }
    }
    if audio.speech.is_some() {
        if voice_config.play_command.is_empty() {
            report.warn(
                "voice playback",
                "[speech] is configured but voice.play_command is not set",
                "set voice.play_command in models.toml (e.g. [\"mpv\", \"--no-terminal\", \"-\"]) \
                 to hear replies",
            );
        } else {
            report.ok(
                "voice playback",
                format!("play_command: {}", voice_config.play_command.join(" ")),
            );
        }
    }
}

/// One `[transcription]`/`[speech]` endpoint's key-resolution readiness.
fn check_voice_endpoint(
    report: &mut Report,
    label: &str,
    config: &codypendent_runtime::models::AudioModelConfig,
    auth: &codypendent_runtime::auth::AuthStore,
    table: &str,
    env_hint: &str,
) {
    let keyless = config.api_key_env.trim().is_empty();
    let has_stored_key = auth.get(table).filter(|key| !key.is_empty()).is_some();
    let env_set = !keyless && std::env::var(&config.api_key_env).is_ok();
    let locality = if config.local {
        "local".to_string()
    } else {
        "remote — governed by routing.toml's policy.max_off_device".to_string()
    };

    if keyless || has_stored_key || env_set {
        report.ok(
            label,
            format!("{} · model {} · {locality}", config.base_url, config.model),
        );
    } else {
        // A key saved through `/keys` (an `auth.json` entry named for the
        // table) outranks the env var and is not tied to any process's
        // environment; the env var remains the alternative, and it must be
        // exported in the RIGHT process, which `doctor` cannot prove from here.
        report.warn(
            label,
            format!(
                "{} · model {} — no key saved in auth.json and {} is not set in doctor's own \
                 environment",
                config.base_url, config.model, config.api_key_env
            ),
            &format!(
                "save the key in the TUI's `/keys` overlay (it now lists a row per configured \
                 voice endpoint), or export {} before starting — {env_hint}",
                config.api_key_env
            ),
        );
    }
}

/// Whether a base URL points at the loopback interface (a local model server).
fn is_local_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "::1"]
        .iter()
        .any(|h| lower.contains(h))
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

    // -----------------------------------------------------------------
    // F6: `doctor` must have voice checks (it had none at all before this).
    // -----------------------------------------------------------------

    fn paths_for(dir: &std::path::Path) -> RuntimePaths {
        let paths = RuntimePaths::from_data_dir(dir.to_path_buf());
        paths.ensure_directories().expect("directories");
        paths
    }

    #[test]
    fn unconfigured_voice_is_a_single_ok_check_not_silence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for(dir.path());
        let mut report = Report::default();
        check_voice(&mut report, &paths);

        let voice: Vec<&Check> = report.items.iter().filter(|c| c.name == "voice").collect();
        assert_eq!(voice.len(), 1);
        assert_eq!(voice[0].status, Status::Ok);
        assert!(
            report.items.iter().all(|c| c.name != "voice recorder"),
            "no [transcription] means a missing recorder is moot, not worth a row"
        );
    }

    #[test]
    fn a_malformed_voice_table_fails_loudly_instead_of_disabling_silently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for(dir.path());
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[transcription]\nbase_url =",
        )
        .expect("write");
        let mut report = Report::default();
        check_voice(&mut report, &paths);

        let voice = report
            .items
            .iter()
            .find(|c| c.name == "voice")
            .expect("a voice row");
        assert_eq!(voice.status, Status::Fail);
    }

    #[test]
    fn a_keyless_endpoint_is_ok_with_no_credential_anywhere() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for(dir.path());
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[transcription]\nbase_url = \"http://127.0.0.1:8080/v1\"\n\
             model = \"whisper-cpp\"\nlocal = true\n",
        )
        .expect("write");
        let mut report = Report::default();
        check_voice(&mut report, &paths);

        let row = report
            .items
            .iter()
            .find(|c| c.name == "voice input (STT)")
            .expect("an STT row");
        assert_eq!(row.status, Status::Ok, "{row:?}");
    }

    #[test]
    fn a_stored_auth_key_satisfies_the_check_without_touching_the_environment() {
        // Exercises the SAME resolution path `AudioTranscriber`/`AudioSynthesizer`
        // use (`auth.get(table)`), deterministically — no process-env mutation.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for(dir.path());
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[speech]\nbase_url = \"https://api.example.invalid/v1\"\n\
             model = \"tts-1\"\napi_key_env = \"CODYPENDENT_TEST_DOCTOR_TTS_KEY\"\n",
        )
        .expect("write");
        let mut auth = codypendent_runtime::auth::AuthStore::default();
        auth.set("speech", "sk-stored-in-auth-json");
        auth.save(&paths.data_dir).expect("save auth.json");

        let mut report = Report::default();
        check_voice(&mut report, &paths);
        let row = report
            .items
            .iter()
            .find(|c| c.name == "voice output (TTS)")
            .expect("a TTS row");
        assert_eq!(row.status, Status::Ok, "{row:?}");
    }

    #[test]
    fn an_unresolvable_key_warns_and_names_the_env_var_and_the_keys_limitation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for(dir.path());
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[transcription]\nbase_url = \"https://api.groq.com/openai/v1\"\n\
             model = \"whisper-large-v3-turbo\"\n\
             api_key_env = \"CODYPENDENT_TEST_DOCTOR_UNSET_XYZ_12345\"\n",
        )
        .expect("write");
        let mut report = Report::default();
        check_voice(&mut report, &paths);

        let row = report
            .items
            .iter()
            .find(|c| c.name == "voice input (STT)")
            .expect("an STT row");
        assert_eq!(row.status, Status::Warn, "{row:?}");
        assert!(row
            .message
            .contains("CODYPENDENT_TEST_DOCTOR_UNSET_XYZ_12345"));
        let hint = row.hint.as_deref().unwrap_or_default();
        assert!(hint.contains("/keys"), "{hint}");
        assert!(
            hint.contains("daemon"),
            "STT names the DAEMON's environment: {hint}"
        );
    }

    #[test]
    fn a_speech_endpoint_names_the_tui_process_not_the_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for(dir.path());
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[speech]\nbase_url = \"https://api.openai.com/v1\"\nmodel = \"tts-1\"\n\
             api_key_env = \"CODYPENDENT_TEST_DOCTOR_UNSET_XYZ_12345\"\n",
        )
        .expect("write");
        let mut report = Report::default();
        check_voice(&mut report, &paths);

        let row = report
            .items
            .iter()
            .find(|c| c.name == "voice output (TTS)")
            .expect("a TTS row");
        assert_eq!(row.status, Status::Warn, "{row:?}");
        let hint = row.hint.as_deref().unwrap_or_default();
        assert!(
            hint.contains("TUI"),
            "TTS names the TUI's environment: {hint}"
        );
    }

    #[test]
    fn transcription_configured_always_yields_a_recorder_row() {
        // The concrete verdict (ok/warn) depends on the ambient $PATH, which
        // this test must not assume anything about — only that the row EXISTS
        // whenever [transcription] is configured, so a user always gets an
        // answer instead of nothing.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for(dir.path());
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[transcription]\nbase_url = \"http://127.0.0.1:8080/v1\"\nmodel = \"whisper-cpp\"\n\
             local = true\n",
        )
        .expect("write");
        let mut report = Report::default();
        check_voice(&mut report, &paths);
        assert!(report.items.iter().any(|c| c.name == "voice recorder"));
    }

    #[test]
    fn speech_configured_without_a_play_command_warns_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for(dir.path());
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[speech]\nbase_url = \"http://127.0.0.1:8080/v1\"\nmodel = \"tts\"\nlocal = true\n",
        )
        .expect("write");
        let mut report = Report::default();
        check_voice(&mut report, &paths);
        let row = report
            .items
            .iter()
            .find(|c| c.name == "voice playback")
            .expect("a playback row");
        assert_eq!(row.status, Status::Warn, "{row:?}");
        assert!(row.message.contains("play_command"));
    }
}
