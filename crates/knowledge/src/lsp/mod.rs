//! Live LSP diagnostics (Phase 4 follow-up: the "live language-server
//! spawn" half of Chapter 07's semantic tier). One process-wide manager
//! lazily spawns servers per (server, workspace-root), caches clients,
//! marks broken pairs so a failing server is tried once, and exposes the
//! one question the write tools ask: "fresh diagnostics for this file".

pub mod client;
pub mod servers;
pub mod transport;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::adapter::{on_path, DiagnosticSeverity};
use client::canonical_or_original;
pub use client::{LspClient, LspDiagnostic};

/// The seam the runtime's write tools consume (mirrors `CodeGraphQueries`:
/// a knowledge-owned trait held as `Option<Arc<dyn …>>` on the runtime).
#[async_trait]
pub trait LiveDiagnostics: Send + Sync {
    /// Touch `file` in every live server responsible for it, wait (bounded)
    /// for fresh diagnostics, and return them. Empty when no server covers
    /// the file, none is installed, spawn failed, or the wait timed out —
    /// this method NEVER errors and NEVER blocks beyond its internal bounds.
    async fn file_diagnostics(&self, file: &Path, worktree: &Path) -> Vec<LspDiagnostic>;
}

pub struct LspManager {
    clients: Mutex<HashMap<(String, PathBuf), Arc<LspClient>>>,
    broken: Mutex<HashSet<(String, PathBuf)>>,
    initializations:
        Mutex<HashMap<(String, PathBuf), Arc<tokio::sync::OnceCell<Option<Arc<LspClient>>>>>>,
}

impl std::fmt::Debug for LspManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspManager").finish_non_exhaustive()
    }
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LspManager {
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            broken: Mutex::new(HashSet::new()),
            initializations: Mutex::new(HashMap::new()),
        }
    }

    fn client_key(spec: &servers::ServerSpec, root: &Path) -> (String, PathBuf) {
        (spec.id.to_string(), canonical_or_original(root))
    }

    fn cache_client(
        &self,
        spec: &servers::ServerSpec,
        root: &Path,
        client: Arc<LspClient>,
    ) -> Arc<LspClient> {
        self.clients
            .lock()
            .unwrap()
            .entry(Self::client_key(spec, root))
            .or_insert(client)
            .clone()
    }

    async fn client_for_with<F, Fut>(
        &self,
        spec: &servers::ServerSpec,
        root: &Path,
        initialize: F,
    ) -> Option<Arc<LspClient>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<Arc<LspClient>>>,
    {
        let key = Self::client_key(spec, root);
        if self.broken.lock().unwrap().contains(&key) {
            return None;
        }
        if let Some(client) = self.clients.lock().unwrap().get(&key) {
            return Some(client.clone());
        }

        let flight = self
            .initializations
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_default()
            .clone();
        flight
            .get_or_init(|| async {
                match initialize().await {
                    Ok(client) => Some(self.cache_client(spec, root, client)),
                    Err(err) => {
                        tracing::warn!(
                            server = spec.id,
                            root = %root.display(),
                            error = %err,
                            "failed to spawn LSP server; marking broken for this session"
                        );
                        self.broken.lock().unwrap().insert(key);
                        None
                    }
                }
            })
            .await
            .clone()
    }

    /// Lazy client for (spec, root): reuse, or spawn+initialize; on failure
    /// mark broken and answer None forever after (per manager lifetime).
    pub async fn client_for(
        &self,
        spec: &servers::ServerSpec,
        root: &Path,
    ) -> Option<Arc<LspClient>> {
        let canon_root = canonical_or_original(root);
        let key = Self::client_key(spec, &canon_root);

        if self.broken.lock().unwrap().contains(&key) {
            return None;
        }
        if let Some(client) = self.clients.lock().unwrap().get(&key) {
            return Some(client.clone());
        }

        if !on_path(spec.binary) {
            return None;
        }

        let init = if spec.id == "pyright" {
            servers::pyright_initialization(&canon_root)
        } else {
            serde_json::json!({})
        };

        let args: Vec<String> = servers::spawn_args(spec)
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        self.client_for_with(spec, &canon_root, || async {
            LspClient::spawn(Path::new(spec.binary), &args, &canon_root, init)
                .await
                .map(Arc::new)
        })
        .await
    }
}

#[async_trait]
impl LiveDiagnostics for LspManager {
    async fn file_diagnostics(&self, file: &Path, worktree: &Path) -> Vec<LspDiagnostic> {
        let file_canon = canonical_or_original(file);
        let worktree_canon = canonical_or_original(worktree);

        let ext = match file_canon.extension().and_then(|e| e.to_str()) {
            Some(e) => format!(".{e}"),
            None => return Vec::new(),
        };

        let mut all_diags = Vec::new();

        for spec in servers::ROSTER {
            if spec.extensions.contains(&ext.as_str()) {
                let root_opt = match spec.id {
                    "rust-analyzer" => servers::rust_analyzer_root(&file_canon, &worktree_canon),
                    "pyright" => servers::pyright_root(&file_canon, &worktree_canon),
                    "typescript" => servers::typescript_root(&file_canon, &worktree_canon),
                    "gopls" => servers::gopls_root(&file_canon, &worktree_canon),
                    "clangd" => servers::clangd_root(&file_canon, &worktree_canon),
                    _ => None,
                };

                if let Some(root) = root_opt {
                    if let Some(client) = self.client_for(spec, &root).await {
                        let after = tokio::time::Instant::now();
                        if let Ok(version) = client.touch(&file_canon).await {
                            client
                                .wait_for_diagnostics(&file_canon, version, after)
                                .await;
                            let diags = client.diagnostics_for(&file_canon).await;
                            all_diags.extend(diags);
                        }
                    }
                }
            }
        }

        all_diags
    }
}

/// Reference `diagnostic.ts` `report`: severity Error only, cap 20 with
/// `... and N more`, 1-based positions. `None` when there are no errors.
pub const MAX_DIAGNOSTICS_PER_FILE: usize = 20;

pub fn report(file: &Path, issues: &[LspDiagnostic]) -> Option<String> {
    let errors: Vec<&LspDiagnostic> = issues
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .collect();

    if errors.is_empty() {
        return None;
    }

    let mut out = format!("<diagnostics file=\"{}\">\n", file.display());
    let total = errors.len();
    let display_count = total.min(MAX_DIAGNOSTICS_PER_FILE);

    for diag in &errors[..display_count] {
        out.push_str(&format!(
            "ERROR [{}:{}] {}\n",
            diag.line + 1,
            diag.character + 1,
            diag.message
        ));
    }

    if total > MAX_DIAGNOSTICS_PER_FILE {
        let remaining = total - MAX_DIAGNOSTICS_PER_FILE;
        out.push_str(&format!("... and {remaining} more\n"));
    }

    out.push_str("</diagnostics>");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::duplex;

    async fn in_memory_client(root: &Path) -> Arc<LspClient> {
        let (client_io, server_io) = duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (server_read, server_write) = tokio::io::split(server_io);
        let client_transport = transport::Transport::new(client_read, client_write);
        let mut server_transport = transport::Transport::new(server_read, server_write);

        let client = LspClient::from_transport(client_transport, root, serde_json::json!({}), None);
        let server = async {
            let initialize = server_transport.read().await.unwrap();
            server_transport
                .respond(
                    initialize.id.unwrap(),
                    serde_json::json!({"capabilities": {}}),
                )
                .await
                .unwrap();
            let initialized = server_transport.read().await.unwrap();
            assert_eq!(initialized.method.as_deref(), Some("initialized"));
        };
        let (client, ()) = tokio::join!(client, server);
        Arc::new(client.unwrap())
    }

    #[test]
    fn report_matches_reference_format() {
        let file = Path::new("src/main.rs");
        let issues = vec![LspDiagnostic {
            line: 4,
            character: 10,
            severity: DiagnosticSeverity::Error,
            message: "cannot find value `x` in this scope".to_string(),
            source: Some("rustc".to_string()),
        }];

        let rendered = report(file, &issues).unwrap();
        assert_eq!(
            rendered,
            "<diagnostics file=\"src/main.rs\">\nERROR [5:11] cannot find value `x` in this scope\n</diagnostics>"
        );
    }

    #[test]
    fn report_caps_at_twenty_with_more_suffix() {
        let file = Path::new("src/lib.rs");
        let issues: Vec<LspDiagnostic> = (0..25)
            .map(|i| LspDiagnostic {
                line: i,
                character: 0,
                severity: DiagnosticSeverity::Error,
                message: format!("error {i}"),
                source: None,
            })
            .collect();

        let rendered = report(file, &issues).unwrap();
        assert!(rendered.contains("ERROR [1:1] error 0\n"));
        assert!(rendered.contains("ERROR [20:1] error 19\n"));
        assert!(!rendered.contains("ERROR [21:1] error 20\n"));
        assert!(rendered.contains("... and 5 more\n"));
    }

    #[test]
    fn report_is_none_without_errors() {
        let file = Path::new("src/main.rs");
        let issues = vec![
            LspDiagnostic {
                line: 0,
                character: 0,
                severity: DiagnosticSeverity::Warning,
                message: "unused variable".to_string(),
                source: None,
            },
            LspDiagnostic {
                line: 1,
                character: 0,
                severity: DiagnosticSeverity::Info,
                message: "hint info".to_string(),
                source: None,
            },
        ];

        assert_eq!(report(file, &issues), None);
    }

    #[tokio::test]
    async fn broken_server_is_not_respawned() {
        let manager = LspManager::new();
        let fake_spec = servers::ServerSpec {
            id: "nonexistent-server",
            extensions: &[".xyz"],
            binary: "definitely-not-installed-binary-12345",
        };
        let tmp = tempfile::tempdir().unwrap();

        let res1 = manager.client_for(&fake_spec, tmp.path()).await;
        assert!(res1.is_none());

        // Second call should return None immediately via broken set or on_path
        let res2 = manager.client_for(&fake_spec, tmp.path()).await;
        assert!(res2.is_none());
    }

    #[tokio::test]
    async fn python_files_in_one_workspace_reuse_one_canonical_root_client() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let package = workspace.join("package");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            workspace.join("pyproject.toml"),
            "[project]\nname = 'test'\n",
        )
        .unwrap();
        let first_file = workspace.join("main.py");
        let second_file = package.join("models.py");
        std::fs::write(&first_file, "print('first')\n").unwrap();
        std::fs::write(&second_file, "print('second')\n").unwrap();

        let first_root = servers::pyright_root(&first_file, &workspace).unwrap();
        let second_root = servers::pyright_root(&second_file, &workspace).unwrap();
        let manager = LspManager::new();
        let owner = in_memory_client(&first_root).await;
        manager.cache_client(&servers::PYRIGHT, &first_root, owner.clone());

        let first_request = manager
            .client_for(&servers::PYRIGHT, &first_root)
            .await
            .unwrap();
        let second_request = manager
            .client_for(&servers::PYRIGHT, &second_root)
            .await
            .unwrap();

        assert_eq!(
            canonical_or_original(&first_root),
            canonical_or_original(&workspace)
        );
        assert_eq!(
            canonical_or_original(&second_root),
            canonical_or_original(&workspace)
        );
        assert!(Arc::ptr_eq(&owner, &first_request));
        assert!(Arc::ptr_eq(&first_request, &second_request));
        assert_eq!(manager.clients.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn concurrent_cache_misses_share_one_initialization_and_client() {
        let manager = Arc::new(LspManager::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        let tmp = tempfile::tempdir().unwrap();
        let spec = servers::ServerSpec {
            id: "test-server",
            extensions: &[".test"],
            binary: "unused-test-binary",
        };
        let root = tmp.path().to_path_buf();

        let request = |manager: Arc<LspManager>, attempts: Arc<AtomicUsize>| {
            let root = root.clone();
            async move {
                let initialization_root = root.clone();
                manager
                    .client_for_with(&spec, &root, || async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                        Ok(in_memory_client(&initialization_root).await)
                    })
                    .await
            }
        };

        let (first, second) = tokio::join!(
            request(manager.clone(), attempts.clone()),
            request(manager.clone(), attempts.clone())
        );
        let first = first.unwrap();
        let second = second.unwrap();

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(manager.clients.lock().unwrap().len(), 1);
    }
}
