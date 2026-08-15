//! One live language-server client. Ported from opencode `lsp/client.ts`,
//! push-diagnostics path only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::sync::Notify;

use super::transport::{Incoming, Transport};
use crate::adapter::DiagnosticSeverity;

/// Classify an incoming message during the initialize handshake: it is the
/// RESPONSE to request `req_id` only if it is a response (no `method`, i.e. a
/// result/error) AND carries that id. A server-initiated request that happens to
/// reuse the id still carries a `method`, so it is not mistaken for the response.
fn is_init_response(msg: &Incoming, req_id: i64) -> bool {
    msg.method.is_none() && msg.id.as_ref() == Some(&serde_json::json!(req_id))
}

/// Reference constants (client.ts lines 13–18).
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(45);
pub const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(150);
pub const DIAGNOSTICS_DOCUMENT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// A published diagnostic for one file, at LSP fidelity (line AND column,
/// both 0-based as received; `report` renders them 1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDiagnostic {
    pub line: u32,
    pub character: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
}

pub struct LspClient {
    state: Arc<Mutex<ClientState>>,
    outbound_tx: mpsc::Sender<OutboundMsg>,
    child: Option<Arc<tokio::sync::Mutex<Child>>>,
    #[allow(dead_code)]
    root: PathBuf,
}

#[allow(dead_code)]
enum OutboundMsg {
    Request { method: String, params: Value },
    Notify { method: String, params: Value },
    Respond { id: Value, result: Value },
}

struct ClientState {
    /// canonical path -> latest published diagnostics.
    push: HashMap<PathBuf, Vec<LspDiagnostic>>,
    /// canonical path -> (publish instant, version if the server echoed one).
    published: HashMap<PathBuf, (tokio::time::Instant, Option<i64>)>,
    /// canonical path -> open-document version counter.
    files: HashMap<PathBuf, i64>,
    /// Wakes `wait_for_diagnostics` on every publish.
    notify: Arc<Notify>,
}

impl LspClient {
    /// Spawn `command` with `cwd = root`, run the initialize handshake
    /// (rootUri + workspaceFolders + publishDiagnostics capability +
    /// `initialization` options) under [`INITIALIZE_TIMEOUT`], send
    /// `initialized`, then start the reader/writer tasks that fold
    /// `textDocument/publishDiagnostics` into the state (answering
    /// `workspace/configuration` with the initialization options and any
    /// other server request with `null`). stderr is drained to /dev/null.
    pub async fn spawn(
        command: &Path,
        args: &[String],
        root: &Path,
        initialization: Value,
    ) -> anyhow::Result<Self> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("could not spawn {}", command.display()))?;

        let stdin = child.stdin.take().context("child stdin was not piped")?;
        let stdout = child.stdout.take().context("child stdout was not piped")?;

        let transport = Transport::new(stdout, stdin);
        Self::from_transport(
            transport,
            root,
            initialization,
            Some(Arc::new(tokio::sync::Mutex::new(child))),
        )
        .await
    }

    /// Construct an `LspClient` over an arbitrary `Transport` (useful for unit testing over duplex).
    pub async fn from_transport<
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    >(
        mut transport: Transport<R, W>,
        root: &Path,
        initialization: Value,
        child: Option<Arc<tokio::sync::Mutex<Child>>>,
    ) -> anyhow::Result<Self> {
        let root_canon = canonical_or_original(root);
        let root_uri = path_to_uri(&root_canon);

        let init_params = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "window": {
                    "workDoneProgress": true
                },
                "workspace": {
                    "configuration": true,
                    "didChangeWatchedFiles": {
                        "dynamicRegistration": true
                    }
                },
                "textDocument": {
                    "synchronization": {
                        "dynamicRegistration": false,
                        "willSave": false,
                        "willSaveWaitUntil": false,
                        "didSave": true
                    },
                    "publishDiagnostics": {
                        "relatedInformation": false,
                        "versionSupport": true,
                        "tagSupport": { "valueSet": [1, 2] }
                    }
                }
            },
            "initializationOptions": initialization,
            "workspaceFolders": [
                {
                    "uri": root_uri,
                    "name": "workspace"
                }
            ]
        });

        // Initialize handshake under INITIALIZE_TIMEOUT
        let init_fut = async {
            let req_id = transport
                .request("initialize", init_params)
                .await
                .context("failed sending initialize request")?;

            loop {
                let msg = transport
                    .read()
                    .await
                    .context("failed reading initialize response")?;
                // A message is the initialize RESPONSE only if it is a response
                // (a result/error, i.e. no `method`) carrying our id. A
                // server-initiated request that happens to reuse this id still
                // carries a `method` and must NOT be mistaken for the response.
                if is_init_response(&msg, req_id) {
                    break;
                }
                if msg.method.is_some() {
                    if let Some(id) = &msg.id {
                        // Server -> client request (id + method): answer so it
                        // doesn't block, then keep waiting for our response.
                        let _ = transport.respond(id.clone(), serde_json::Value::Null).await;
                    }
                }
            }

            transport
                .notify("initialized", serde_json::json!({}))
                .await
                .context("failed sending initialized notification")?;

            if !initialization.is_null() && initialization != serde_json::json!({}) {
                let _ = transport
                    .notify(
                        "workspace/didChangeConfiguration",
                        serde_json::json!({ "settings": initialization }),
                    )
                    .await;
            }
            Ok::<(), anyhow::Error>(())
        };

        tokio::time::timeout(INITIALIZE_TIMEOUT, init_fut)
            .await
            .context("initialize handshake timed out")??;

        let notify = Arc::new(Notify::new());
        let state = Arc::new(Mutex::new(ClientState {
            push: HashMap::new(),
            published: HashMap::new(),
            files: HashMap::new(),
            notify: notify.clone(),
        }));

        let (outbound_tx, mut outbound_rx) = mpsc::channel::<OutboundMsg>(64);

        let bg_state = state.clone();
        let init_options = initialization.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(outbound) = outbound_rx.recv() => {
                        match outbound {
                            OutboundMsg::Request { method, params } => {
                                let _ = transport.request(&method, params).await;
                            }
                            OutboundMsg::Notify { method, params } => {
                                let _ = transport.notify(&method, params).await;
                            }
                            OutboundMsg::Respond { id, result } => {
                                let _ = transport.respond(id, result).await;
                            }
                        }
                    }
                    res = transport.read() => {
                        match res {
                            Ok(msg) => {
                                handle_incoming_message(&msg, &bg_state, &init_options, &mut transport).await;
                            }
                            Err(_) => {
                                // Transport closed or error
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            state,
            outbound_tx,
            child,
            root: root_canon,
        })
    }

    /// didOpen on first touch (version 0), didChange after (version+1) with
    /// one whole-document content change. Reads the file itself. NEVER
    /// clears cached diagnostics on didChange (client.ts line 564 comment:
    /// servers may not re-emit for unchanged content). Returns the version.
    pub async fn touch(&self, path: &Path) -> anyhow::Result<i64> {
        let canon = canonical_or_original(path);
        let uri = path_to_uri(&canon);
        let content = tokio::fs::read_to_string(&canon)
            .await
            .with_context(|| format!("could not read file {}", canon.display()))?;

        let (is_open, version) = {
            let mut st = self.state.lock().unwrap();
            if let Some(v) = st.files.get_mut(&canon) {
                *v += 1;
                (true, *v)
            } else {
                st.files.insert(canon.clone(), 0);
                (false, 0)
            }
        };

        if !is_open {
            let params = serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id_for_path(path),
                    "version": version,
                    "text": content
                }
            });
            let _ = self
                .outbound_tx
                .send(OutboundMsg::Notify {
                    method: "textDocument/didOpen".to_string(),
                    params,
                })
                .await;
        } else {
            let change_params = serde_json::json!({
                "changes": [
                    {
                        "uri": uri,
                        "type": 2
                    }
                ]
            });
            let _ = self
                .outbound_tx
                .send(OutboundMsg::Notify {
                    method: "workspace/didChangeWatchedFiles".to_string(),
                    params: change_params,
                })
                .await;

            let did_change_params = serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "version": version
                },
                "contentChanges": [
                    {
                        "text": content
                    }
                ]
            });
            let _ = self
                .outbound_tx
                .send(OutboundMsg::Notify {
                    method: "textDocument/didChange".to_string(),
                    params: did_change_params,
                })
                .await;
        }

        let did_save_params = serde_json::json!({
            "textDocument": {
                "uri": uri
            }
        });
        let _ = self
            .outbound_tx
            .send(OutboundMsg::Notify {
                method: "textDocument/didSave".to_string(),
                params: did_save_params,
            })
            .await;

        Ok(version)
    }

    /// Block until a publish for `path` arrives that is fresh — version
    /// matches `version`, or it landed at/after `after` — then debounce
    /// [`DIAGNOSTICS_DEBOUNCE`] so successive publishes settle; give up at
    /// [`DIAGNOSTICS_DOCUMENT_WAIT_TIMEOUT`]. Never errors: timing out just
    /// means "whatever is cached is what you get".
    pub async fn wait_for_diagnostics(
        &self,
        path: &Path,
        version: i64,
        after: tokio::time::Instant,
    ) {
        let canon = canonical_or_original(path);
        let notify = {
            let st = self.state.lock().unwrap();
            st.notify.clone()
        };

        let wait_fut = async {
            loop {
                // Register the waiter BEFORE checking published state. The
                // notifier uses `notify_waiters()`, which wakes only
                // already-registered waiters and stores no permit; a freshly
                // created (unenabled) `Notified` future would miss a publish that
                // lands between this check and the `.await`, stalling up to the
                // 5s timeout. `enable()` registers the waiter first, closing the
                // missed-wakeup race (tokio's documented pattern).
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                {
                    let st = self.state.lock().unwrap();
                    if let Some((instant, ver)) = st.published.get(&canon) {
                        if *ver == Some(version) || *instant >= after {
                            break;
                        }
                    }
                }
                notified.await;
            }
            tokio::time::sleep(DIAGNOSTICS_DEBOUNCE).await;
        };

        let _ = tokio::time::timeout(DIAGNOSTICS_DOCUMENT_WAIT_TIMEOUT, wait_fut).await;
    }

    /// The latest published diagnostics for `path` (empty when none).
    pub async fn diagnostics_for(&self, path: &Path) -> Vec<LspDiagnostic> {
        let canon = canonical_or_original(path);
        let st = self.state.lock().unwrap();
        st.push.get(&canon).cloned().unwrap_or_default()
    }

    /// `shutdown` request + `exit` notification, then kill on timeout.
    pub async fn shutdown(&self) {
        let _ = self
            .outbound_tx
            .send(OutboundMsg::Request {
                method: "shutdown".to_string(),
                params: serde_json::json!(null),
            })
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = self
            .outbound_tx
            .send(OutboundMsg::Notify {
                method: "exit".to_string(),
                params: serde_json::json!(null),
            })
            .await;

        if let Some(child_arc) = &self.child {
            let mut child = child_arc.lock().await;
            let _ = child.kill().await;
        }
    }
}

async fn handle_incoming_message<
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
>(
    msg: &Incoming,
    state: &Arc<Mutex<ClientState>>,
    init_options: &Value,
    transport: &mut Transport<R, W>,
) {
    if let Some(method) = &msg.method {
        if method == "textDocument/publishDiagnostics" {
            if let Some(params) = &msg.params {
                if let Some(uri_str) = params.get("uri").and_then(Value::as_str) {
                    let path = uri_to_path(uri_str);
                    let version = params.get("version").and_then(Value::as_i64);
                    let mut diags = Vec::new();
                    if let Some(items) = params.get("diagnostics").and_then(Value::as_array) {
                        for item in items {
                            let severity_num =
                                item.get("severity").and_then(Value::as_u64).unwrap_or(1);
                            let severity = match severity_num {
                                1 => DiagnosticSeverity::Error,
                                2 => DiagnosticSeverity::Warning,
                                3 => DiagnosticSeverity::Info,
                                _ => DiagnosticSeverity::Hint,
                            };
                            let line = item
                                .pointer("/range/start/line")
                                .and_then(Value::as_u64)
                                .unwrap_or(0) as u32;
                            let character = item
                                .pointer("/range/start/character")
                                .and_then(Value::as_u64)
                                .unwrap_or(0) as u32;
                            let message = item
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let source = item
                                .get("source")
                                .and_then(Value::as_str)
                                .map(ToString::to_string);

                            diags.push(LspDiagnostic {
                                line,
                                character,
                                severity,
                                message,
                                source,
                            });
                        }
                    }

                    {
                        let mut st = state.lock().unwrap();
                        st.push.insert(path.clone(), diags);
                        st.published
                            .insert(path, (tokio::time::Instant::now(), version));
                        st.notify.notify_waiters();
                    }
                }
            }
        } else if let Some(id) = &msg.id {
            // Server -> client request
            if method == "workspace/configuration" {
                let _ = transport
                    .respond(id.clone(), serde_json::json!([init_options]))
                    .await;
            } else {
                let _ = transport.respond(id.clone(), Value::Null).await;
            }
        }
    }
}

pub(crate) fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(crate) fn path_to_uri(path: &Path) -> String {
    let canon = canonical_or_original(path);
    url::Url::from_file_path(&canon)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", canon.display()))
}

pub(crate) fn uri_to_path(uri: &str) -> PathBuf {
    if let Ok(url) = url::Url::parse(uri) {
        if let Ok(path) = url.to_file_path() {
            return canonical_or_original(&path);
        }
    }
    if let Some(stripped) = uri.strip_prefix("file://") {
        #[cfg(windows)]
        let stripped = stripped.strip_prefix('/').unwrap_or(stripped);
        canonical_or_original(Path::new(stripped))
    } else {
        canonical_or_original(Path::new(uri))
    }
}

fn language_id_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => "rust",
        Some("py") | Some("pyi") => "python",
        Some("ts") | Some("tsx") => "typescript",
        Some("js") | Some("jsx") => "javascript",
        Some("json") => "json",
        _ => "plaintext",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn incoming(id: Option<i64>, method: Option<&str>) -> Incoming {
        Incoming {
            id: id.map(|i| serde_json::json!(i)),
            method: method.map(ToString::to_string),
            params: None,
            result: None,
            error: None,
        }
    }

    /// FIX 6a: only a RESPONSE (no `method`) carrying our id is the init
    /// response. A server-initiated request that collides on the id — or a
    /// response with a different id, or a notification — is not.
    #[test]
    fn init_response_is_a_response_with_the_matching_id_not_a_colliding_request() {
        // The actual initialize response: result/error, no method, our id.
        assert!(is_init_response(&incoming(Some(1), None), 1));
        // A server-initiated request that reused id 1 (has a method) is NOT it.
        assert!(!is_init_response(
            &incoming(Some(1), Some("window/showMessageRequest")),
            1
        ));
        // A response to a different request id is NOT it.
        assert!(!is_init_response(&incoming(Some(2), None), 1));
        // A notification (method, no id) is NOT it.
        assert!(!is_init_response(
            &incoming(None, Some("textDocument/publishDiagnostics")),
            1
        ));
    }

    #[tokio::test]
    async fn initialize_handshake_sends_root_and_capabilities() {
        tokio::time::pause();
        let (client_io, server_io) = duplex(4096);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let client_t = Transport::new(cr, cw);
        let mut server_t = Transport::new(sr, sw);

        let root = Path::new("/tmp/test_root");
        let init_options = serde_json::json!({"pythonPath": "/usr/bin/python3"});

        let client_fut = LspClient::from_transport(client_t, root, init_options, None);

        let server_fut = async {
            let msg = server_t.read().await.unwrap();
            assert_eq!(msg.method.as_deref(), Some("initialize"));
            let params = msg.params.unwrap();
            assert!(params.get("rootUri").is_some());
            assert!(params.get("capabilities").is_some());

            server_t
                .respond(
                    msg.id.unwrap(),
                    serde_json::json!({
                        "capabilities": {}
                    }),
                )
                .await
                .unwrap();

            let init_msg = server_t.read().await.unwrap();
            assert_eq!(init_msg.method.as_deref(), Some("initialized"));
        };

        let (client_res, _) = tokio::join!(client_fut, server_fut);
        let client = client_res.unwrap();
        assert_eq!(
            client.diagnostics_for(Path::new("/tmp/foo.rs")).await,
            vec![]
        );
    }

    #[tokio::test]
    async fn touch_sends_didopen_then_didchange_with_versions() {
        tokio::time::pause();
        let (client_io, server_io) = duplex(4096);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let client_t = Transport::new(cr, cw);
        let mut server_t = Transport::new(sr, sw);

        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "fn main() {}\n").unwrap();

        let client_fut =
            LspClient::from_transport(client_t, tmp.path(), serde_json::json!({}), None);

        let server_handshake = async {
            let msg = server_t.read().await.unwrap();
            server_t
                .respond(msg.id.unwrap(), serde_json::json!({"capabilities": {}}))
                .await
                .unwrap();
            let _ = server_t.read().await.unwrap();
        };

        let (client_res, _) = tokio::join!(client_fut, server_handshake);
        let client = client_res.unwrap();

        // 1. First touch -> didOpen (version 0)
        let v0 = client.touch(&file_path).await.unwrap();
        assert_eq!(v0, 0);

        let open_msg = server_t.read().await.unwrap();
        assert_eq!(open_msg.method.as_deref(), Some("textDocument/didOpen"));
        let open_doc = open_msg.params.unwrap();
        assert_eq!(open_doc.pointer("/textDocument/version").unwrap(), 0);

        let save_msg = server_t.read().await.unwrap();
        assert_eq!(save_msg.method.as_deref(), Some("textDocument/didSave"));

        // 2. Second touch -> didChange (version 1)
        std::fs::write(&file_path, "fn main() { let x = 1; }\n").unwrap();
        let v1 = client.touch(&file_path).await.unwrap();
        assert_eq!(v1, 1);

        let watched_msg = server_t.read().await.unwrap();
        assert_eq!(
            watched_msg.method.as_deref(),
            Some("workspace/didChangeWatchedFiles")
        );

        let change_msg = server_t.read().await.unwrap();
        assert_eq!(change_msg.method.as_deref(), Some("textDocument/didChange"));
        let change_doc = change_msg.params.unwrap();
        assert_eq!(change_doc.pointer("/textDocument/version").unwrap(), 1);

        let save_msg2 = server_t.read().await.unwrap();
        assert_eq!(save_msg2.method.as_deref(), Some("textDocument/didSave"));
    }

    #[tokio::test]
    async fn publish_updates_cache_and_wakes_waiter_after_debounce() {
        tokio::time::pause();
        let (client_io, server_io) = duplex(4096);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let client_t = Transport::new(cr, cw);
        let mut server_t = Transport::new(sr, sw);

        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("main.rs");
        std::fs::write(&file_path, "syntax error").unwrap();

        let client_fut =
            LspClient::from_transport(client_t, tmp.path(), serde_json::json!({}), None);
        let server_handshake = async {
            let msg = server_t.read().await.unwrap();
            server_t
                .respond(msg.id.unwrap(), serde_json::json!({"capabilities": {}}))
                .await
                .unwrap();
            let _ = server_t.read().await.unwrap();
        };

        let (client_res, _) = tokio::join!(client_fut, server_handshake);
        let client = client_res.unwrap();

        let canon = canonical_or_original(&file_path);
        let uri = path_to_uri(&canon);

        let now = tokio::time::Instant::now();
        let client_wait = client.wait_for_diagnostics(&file_path, 0, now);

        let server_send_publish = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            server_t
                .notify(
                    "textDocument/publishDiagnostics",
                    serde_json::json!({
                        "uri": uri,
                        "version": 0,
                        "diagnostics": [
                            {
                                "range": {
                                    "start": { "line": 0, "character": 7 },
                                    "end": { "line": 0, "character": 12 }
                                },
                                "severity": 1,
                                "message": "expected semicolon",
                                "source": "rustc"
                            }
                        ]
                    }),
                )
                .await
                .unwrap();
        };

        tokio::join!(client_wait, server_send_publish);

        let diags = client.diagnostics_for(&file_path).await;
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 0);
        assert_eq!(diags[0].character, 7);
        assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
        assert_eq!(diags[0].message, "expected semicolon");
    }

    #[tokio::test]
    async fn stale_version_publish_does_not_release_the_wait() {
        tokio::time::pause();
        let (client_io, server_io) = duplex(4096);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let client_t = Transport::new(cr, cw);
        let mut server_t = Transport::new(sr, sw);

        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("main.rs");
        std::fs::write(&file_path, "code").unwrap();

        let client_fut =
            LspClient::from_transport(client_t, tmp.path(), serde_json::json!({}), None);
        let server_handshake = async {
            let msg = server_t.read().await.unwrap();
            server_t
                .respond(msg.id.unwrap(), serde_json::json!({"capabilities": {}}))
                .await
                .unwrap();
            let _ = server_t.read().await.unwrap();
        };

        let (client_res, _) = tokio::join!(client_fut, server_handshake);
        let client = client_res.unwrap();

        let canon = canonical_or_original(&file_path);
        let uri = path_to_uri(&canon);

        let after = tokio::time::Instant::now() + Duration::from_secs(10);
        // Wait for version 2, while server publishes version 1
        let client_wait = client.wait_for_diagnostics(&file_path, 2, after);

        let server_send_stale = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            server_t
                .notify(
                    "textDocument/publishDiagnostics",
                    serde_json::json!({
                        "uri": uri,
                        "version": 1,
                        "diagnostics": []
                    }),
                )
                .await
                .unwrap();
        };

        let start = tokio::time::Instant::now();
        tokio::join!(client_wait, server_send_stale);
        let elapsed = tokio::time::Instant::now() - start;
        assert!(elapsed >= DIAGNOSTICS_DOCUMENT_WAIT_TIMEOUT);
    }

    #[tokio::test]
    async fn wait_times_out_at_five_seconds() {
        tokio::time::pause();
        let (client_io, server_io) = duplex(4096);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let client_t = Transport::new(cr, cw);
        let mut server_t = Transport::new(sr, sw);

        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("main.rs");
        std::fs::write(&file_path, "code").unwrap();

        let client_fut =
            LspClient::from_transport(client_t, tmp.path(), serde_json::json!({}), None);
        let server_handshake = async {
            let msg = server_t.read().await.unwrap();
            server_t
                .respond(msg.id.unwrap(), serde_json::json!({"capabilities": {}}))
                .await
                .unwrap();
            let _ = server_t.read().await.unwrap();
        };

        let (client_res, _) = tokio::join!(client_fut, server_handshake);
        let client = client_res.unwrap();

        let start = tokio::time::Instant::now();
        client
            .wait_for_diagnostics(&file_path, 0, start + Duration::from_secs(1))
            .await;
        let elapsed = tokio::time::Instant::now() - start;
        assert!(elapsed >= DIAGNOSTICS_DOCUMENT_WAIT_TIMEOUT);
    }

    #[tokio::test]
    async fn didchange_does_not_clear_cached_diagnostics() {
        tokio::time::pause();
        let (client_io, server_io) = duplex(4096);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let client_t = Transport::new(cr, cw);
        let mut server_t = Transport::new(sr, sw);

        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("main.rs");
        std::fs::write(&file_path, "code").unwrap();

        let client_fut =
            LspClient::from_transport(client_t, tmp.path(), serde_json::json!({}), None);
        let server_handshake = async {
            let msg = server_t.read().await.unwrap();
            server_t
                .respond(msg.id.unwrap(), serde_json::json!({"capabilities": {}}))
                .await
                .unwrap();
            let _ = server_t.read().await.unwrap();
        };

        let (client_res, _) = tokio::join!(client_fut, server_handshake);
        let client = client_res.unwrap();

        let canon = canonical_or_original(&file_path);
        let uri = path_to_uri(&canon);

        // Pre-populate diagnostics via publish
        server_t
            .notify(
                "textDocument/publishDiagnostics",
                serde_json::json!({
                    "uri": uri,
                    "version": 0,
                    "diagnostics": [
                        {
                            "range": { "start": { "line": 1, "character": 2 } },
                            "severity": 1,
                            "message": "err"
                        }
                    ]
                }),
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(client.diagnostics_for(&file_path).await.len(), 1);

        // Touch again (didChange)
        let _ = client.touch(&file_path).await.unwrap();

        // Cached diagnostics must still be present!
        let diags = client.diagnostics_for(&file_path).await;
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "err");
    }

    #[tokio::test]
    async fn severity_mapping_defaults_to_error() {
        tokio::time::pause();
        let (client_io, server_io) = duplex(4096);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let client_t = Transport::new(cr, cw);
        let mut server_t = Transport::new(sr, sw);

        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("main.rs");
        std::fs::write(&file_path, "code").unwrap();

        let client_fut =
            LspClient::from_transport(client_t, tmp.path(), serde_json::json!({}), None);
        let server_handshake = async {
            let msg = server_t.read().await.unwrap();
            server_t
                .respond(msg.id.unwrap(), serde_json::json!({"capabilities": {}}))
                .await
                .unwrap();
            let _ = server_t.read().await.unwrap();
        };

        let (client_res, _) = tokio::join!(client_fut, server_handshake);
        let client = client_res.unwrap();

        let canon = canonical_or_original(&file_path);
        let uri = path_to_uri(&canon);

        // Publish with missing severity
        server_t
            .notify(
                "textDocument/publishDiagnostics",
                serde_json::json!({
                    "uri": uri,
                    "version": 0,
                    "diagnostics": [
                        {
                            "range": { "start": { "line": 0, "character": 0 } },
                            "message": "missing severity"
                        }
                    ]
                }),
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        let diags = client.diagnostics_for(&file_path).await;
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
    }
}
