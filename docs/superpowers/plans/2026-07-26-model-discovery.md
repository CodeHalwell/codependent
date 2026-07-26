# Model Discovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After picking a provider in the `/provider` picker, query that provider's `<base_url>/models` endpoint and let the user pick a model from a live list (instead of typing the id); providers that can't list fall back to today's free-text flow, and the picker's `Enter` begins the add-model flow while the inert `pending_provider` staging is removed.

**Architecture:** The TUI stays a pure, I/O-free reducer: it emits a new **client-only** `Intent::QueryProviderModels`; the **CLI harness** performs the HTTP GET (the one place the two worlds meet, exactly as it already does file I/O for `Intent::AddModel`) on a spawned task that feeds the result back as a `ReaderSignal::ProviderModels` → `Action::ProviderModelsLoaded`/`ProviderModelsFailed`. On any failure the flow falls back to today's free-text `AddModelId` prompt, carrying any key already entered.

**Tech Stack:** Rust workspace. `codypendent-tui` (pure reducer/render/input — no I/O, no HTTP), `codypendent-cli` (the harness — `reqwest` GET, catalog resolution), `codypendent-providers` (network-free catalog/auth leaf crate). Terminal UI is Ratatui + crossterm. HTTP is `reqwest` (workspace stack). JSON parse is `serde`/`serde_json`.

## Global Constraints

_Every task's requirements implicitly include this section. Values copied verbatim from `docs/superpowers/specs/2026-07-26-model-discovery-design.md`._

- **Client-only:** no protocol/daemon/wire/golden change. `QueryProviderModels` and `AddModel` are both intercepted in the harness drain loop and never become `CommandBody`s — `intent_to_command` keeps its `unreachable!` for them.
- **Pure-reducer TUI (no I/O):** the HTTP GET happens only in the harness; the `tui` crate and the network-free `providers` leaf crate never gain an HTTP dep.
- **Secret hygiene:** the query key is a `SecretKey` (redacted `Debug`, already tested). It crosses to the harness exactly once (in `QueryProviderModels`), is used for the request, and is **never** sent back in an `Action`, logged, put in an error/`reason` string, or rendered (the reducer keeps its own copy in `AddModelQuerying` for the round trip; render masks `AddModelProviderKey`). It reaches disk only via the existing `write_add_model` → `auth.json` (mode `0600`) path.
- **Endpoint:** the list route is `<base_url>/models`, **not** `<base_url>/v1/models` — every catalog `base_url` already carries its version segment. Construction: `format!("{}/models", base_url.trim_end_matches('/'))`. Hardcoding `/v1/models` would double the version and be wrong for z.ai (`/v4`).
- **`reqwest` reuse:** the workspace HTTP stack — root `Cargo.toml`: `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }`. Add `reqwest = { workspace = true }` to `crates/cli/Cargo.toml`. No new HTTP crate.
- **Bounding:** `reqwest::Client::builder().timeout(Duration::from_secs(10))` so a hung endpoint can't wedge the query task.
- **Responsive UI:** the query is a spawned task, not a blocking `await` in the drain loop, so the loop keeps ticking/redrawing and the "Fetching…" state is visible and cancelable.
- **Fallback path always available:** query unreachable / non-2xx / unparseable / auth rejected / empty list → free-text `AddModelId`, carrying any entered key.
- **No caching; no fuzzy/remote search** beyond the provider's own list.
- Preserve routing's data-classification hard-filter and T1/T7 cost honesty (unchanged — discovery only adds a `models.toml` entry through the existing path).
- **Lint gate:** `cargo clippy` must pass **on Linux CI** (`-D warnings`), not only local macOS — a helper reachable only under a platform/`cfg(test)` gate trips the Linux dead-code lint. All new helpers here are used in non-test code paths.
- **Never touch foreign files:** `README.md`, `docs/cli-and-tui-user-guide.md`, `docs/docs/*`, `ROADMAP.md`, `.superpowers/`.

---

## File map

- `crates/tui/src/action.rs` — add `Intent::QueryProviderModels`, `Action::ProviderModelsLoaded`/`ProviderModelsFailed`. (`SecretKey` already lives here, redacted `Debug`.)
- `crates/tui/src/state.rs` — add `ProviderCard.can_list_models`; add overlays `AddModelProviderKey`/`AddModelQuerying`/`AddModelPick`; add `AddModelId.api_key`; add `filter_model_names`; classify new overlays in `input_mode()`; remove `pending_provider`.
- `crates/tui/src/reduce.rs` — the two new `Action` handlers; new overlay `submit_prompt`/`edit_prompt`/`nav` arms; `AddModelId` `api_key` branch; `enter_add_model_flow`; rewrite `begin_add_model` (Tab) + the `ProviderPicker` Enter arm.
- `crates/tui/src/render.rs` — render arms for the three new overlays; remove the `pending_provider` staged marker; provider-picker discoverability hint.
- `crates/tui/src/input.rs` — **no change** (the new overlays route through `input_mode()`).
- `crates/cli/src/tui.rs` — `provider_can_list_models`; `ReaderSignal::ProviderModels` + `select!` mapping; drain-loop interception of `Intent::QueryProviderModels` + spawned GET; `query_provider_models` / `models_url` / `parse_models_response`; thread `query_tx` into `event_loop`; the `intent_to_command` `unreachable!` arm.
- `crates/cli/Cargo.toml` — add `reqwest = { workspace = true }`.

## Task order rationale (read before starting)

The harness interception (Task 3) is sequenced **before** the reducer flow (Task 4) on purpose: Task 4 is what makes `Enter`/`Tab` emit `Intent::QueryProviderModels`, and that intent must already be intercepted in the drain loop (Task 3) or it would fall through to `intent_to_command`'s `unreachable!` and panic. Task 2 adds the `unreachable!` arm (to keep the workspace compiling once the variant exists) but nothing emits the intent at runtime until Task 4.

---

## Task 1: `ProviderCard.can_list_models` + `provider_can_list_models` (the harness gate)

Pure, additive. Adds the harness-computed bool that later drives the `Enter`/`Tab` branch, and its directly-unit-testable derivation. No behavior change yet — the field is carried but unread until Task 4.

**Files:**
- Modify: `crates/tui/src/state.rs` (`ProviderCard` struct, ~723-739)
- Modify: `crates/cli/src/tui.rs` (`load_provider_cards` ~1495-1534; add `provider_can_list_models` next to `provider_requires_key` ~1543; tests ~3461-3531)
- Modify (compile-fix literals): `crates/tui/src/reduce.rs` (`provider_card` test helper ~3580-3595), `crates/tui/src/render.rs` (two `ProviderCard` literals ~4341, ~4349)

**Interfaces:**
- Produces: `codypendent_tui::ProviderCard.can_list_models: bool` (new public field, last in the struct).
- Produces (cli-private): `fn provider_can_list_models(p: &codypendent_providers::Provider) -> bool`.
- Consumes: `codypendent_providers::{Provider, Protocol, AuthMethod}` (real enums: `Protocol::OpenAiChat`, `AuthMethod::{ApiKey, None}`), `Provider.base_url: Option<String>`.

- [ ] **Step 1: Write the failing unit tests for `provider_can_list_models`**

In `crates/cli/src/tui.rs`, in the existing `#[cfg(test)] mod tests`, after the `provider_requires_key` tests (~line 3531, before the closing `}` of the module) add a fixture helper and tests:

```rust
    // -- provider_can_list_models (model-discovery gate) ----------------------

    /// A `Provider` with an explicit protocol + base_url, for exercising
    /// `provider_can_list_models` (which reads all three of protocol, base_url,
    /// and the first auth method).
    fn provider_listable(
        protocol: codypendent_providers::Protocol,
        base_url: Option<&str>,
        auth: Vec<codypendent_providers::AuthMethod>,
    ) -> codypendent_providers::Provider {
        codypendent_providers::Provider {
            id: "test-provider".to_string(),
            name: "Test Provider".to_string(),
            protocol,
            base_url: base_url.map(str::to_string),
            auth,
            extra_headers: Default::default(),
            query_params: Default::default(),
            local: false,
        }
    }

    #[test]
    fn can_list_models_true_for_openai_chat_with_base_url_and_api_key() {
        use codypendent_providers::{AuthMethod, Protocol};
        let p = provider_listable(
            Protocol::OpenAiChat,
            Some("https://api.groq.com/openai/v1"),
            vec![AuthMethod::ApiKey {
                env: vec!["GROQ_API_KEY".to_string()],
                header: "Authorization".to_string(),
                prefix: "Bearer ".to_string(),
            }],
        );
        assert!(provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_true_for_openai_chat_with_base_url_and_no_auth() {
        use codypendent_providers::{AuthMethod, Protocol};
        let p = provider_listable(
            Protocol::OpenAiChat,
            Some("http://localhost:11434/v1"),
            vec![AuthMethod::None],
        );
        assert!(provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_true_for_openai_chat_with_base_url_and_empty_auth() {
        use codypendent_providers::Protocol;
        let p = provider_listable(Protocol::OpenAiChat, Some("http://localhost:1234/v1"), vec![]);
        assert!(provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_false_without_a_base_url() {
        use codypendent_providers::{AuthMethod, Protocol};
        let p = provider_listable(
            Protocol::OpenAiChat,
            None,
            vec![AuthMethod::ApiKey {
                env: vec!["OPENAI_API_KEY".to_string()],
                header: "Authorization".to_string(),
                prefix: "Bearer ".to_string(),
            }],
        );
        assert!(!provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_false_for_a_blank_base_url() {
        use codypendent_providers::{AuthMethod, Protocol};
        let p = provider_listable(Protocol::OpenAiChat, Some("   "), vec![AuthMethod::None]);
        assert!(!provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_false_for_non_openai_chat_protocols() {
        use codypendent_providers::{AuthMethod, Protocol};
        for protocol in [Protocol::Anthropic, Protocol::GeminiNative, Protocol::Acp] {
            let p = provider_listable(
                protocol,
                Some("https://api.anthropic.com"),
                vec![AuthMethod::ApiKey {
                    env: vec!["ANTHROPIC_API_KEY".to_string()],
                    header: "x-api-key".to_string(),
                    prefix: "".to_string(),
                }],
            );
            assert!(!provider_can_list_models(&p), "protocol {protocol:?} must not list");
        }
    }

    #[test]
    fn can_list_models_false_for_cloud_iam_and_oauth() {
        use codypendent_providers::{AuthMethod, Protocol};
        let cloud_iam = provider_listable(
            Protocol::OpenAiChat,
            Some("https://bedrock.example/v1"),
            vec![AuthMethod::CloudIam {
                variant: "aws_sigv4".to_string(),
                env: Default::default(),
                scopes: vec![],
            }],
        );
        assert!(!provider_can_list_models(&cloud_iam));

        let oauth = provider_listable(
            Protocol::OpenAiChat,
            Some("https://oauth.example/v1"),
            vec![AuthMethod::OAuth {
                authorize_url: "https://example.com/authorize".to_string(),
                token_url: "https://example.com/token".to_string(),
                client_id: "client".to_string(),
                scopes: vec![],
                pkce: true,
            }],
        );
        assert!(!provider_can_list_models(&oauth));
    }
```

- [ ] **Step 2: Run the tests to verify they fail (compile error)**

Run: `cargo test -p codypendent-cli provider_can_list_models can_list_models 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'provider_can_list_models' in this scope`.

- [ ] **Step 3: Implement `provider_can_list_models`**

In `crates/cli/src/tui.rs`, immediately after `provider_requires_key` (~line 1546) add:

```rust
/// Whether adding a model from `p` can use a live `/models` list: the protocol
/// is OpenAI-compatible (`OpenAiChat`), a non-blank `base_url` is set, and the
/// first auth method is `ApiKey` or `None` (or there is none at all). Native
/// (Anthropic/Gemini), ACP, cloud-IAM, and OAuth providers — and any without a
/// `base_url` — cannot list here and take the free-text path. A tiny pure
/// expression (no I/O), extracted out of `load_provider_cards` so it is directly
/// unit-testable against the real `codypendent_providers` enums.
fn provider_can_list_models(p: &codypendent_providers::Provider) -> bool {
    use codypendent_providers::{AuthMethod, Protocol};
    matches!(p.protocol, Protocol::OpenAiChat)
        && p.base_url.as_deref().is_some_and(|u| !u.trim().is_empty())
        && matches!(
            p.auth.first(),
            Some(AuthMethod::ApiKey { .. } | AuthMethod::None) | None
        )
}
```

- [ ] **Step 4: Add the `can_list_models` field to `ProviderCard`**

In `crates/tui/src/state.rs`, in `pub struct ProviderCard` (~723-739), after the `requires_key` field add:

```rust
    /// Whether this provider can serve an OpenAI-compatible `/models` list:
    /// protocol is `OpenAiChat`, a `base_url` is set, and the first auth method
    /// is `ApiKey` or `None` (or absent). Set by the harness
    /// (`provider_can_list_models`), mirroring `requires_key`. Drives the
    /// Enter/Tab branch: `true` → live pick-list; `false` → today's free-text flow.
    pub can_list_models: bool,
```

- [ ] **Step 5: Set the field in `load_provider_cards`**

In `crates/cli/src/tui.rs`, in `load_provider_cards`'s `.map(|p| ProviderCard { ... })` (~1508-1532), after the `requires_key: provider_requires_key(p),` line add:

```rust
            // Whether the add-model flow can offer a live `/models` pick-list for
            // this provider (OpenAiChat + base_url + ApiKey/None), vs. the
            // free-text fallback. Extracted to `provider_can_list_models`, unit
            // tested against the real enums.
            can_list_models: provider_can_list_models(p),
```

- [ ] **Step 6: Fix the compile-broken `ProviderCard` literals**

In `crates/tui/src/reduce.rs`, in the `provider_card` test helper (~3587-3594), after `requires_key: auth.starts_with("api-key"),` add — deriving the gate the same string-only way the helper already derives `requires_key`:

```rust
            // Mirrors the harness gate closely enough for reducer tests: an
            // OpenAI-compatible provider with an api-key/none auth badge lists.
            can_list_models: protocol == "openai-chat"
                && (auth.starts_with("api-key") || auth == "none"),
```

In `crates/tui/src/render.rs`, the two `ProviderCard` literals in `provider_picker_snapshot_shows_rows_staged_marker_and_badges` (~4341 groq, ~4349 ollama) each gain a field after `requires_key: ...,`:

```rust
                can_list_models: true,
```

(Both are `openai-chat` with `api-key`/`none`, so both list.)

- [ ] **Step 7: Run the full affected test suites — verify pass**

Run: `cargo test -p codypendent-cli && cargo test -p codypendent-tui 2>&1 | tail -20`
Expected: PASS (new `provider_can_list_models` tests green; all existing tests still green).

- [ ] **Step 8: Clippy + commit**

Run: `cargo clippy -p codypendent-cli -p codypendent-tui --all-targets -- -D warnings`
Expected: no warnings.

```bash
git add crates/tui/src/state.rs crates/cli/src/tui.rs crates/tui/src/reduce.rs crates/tui/src/render.rs
git commit -m "feat(tui): add ProviderCard.can_list_models + provider_can_list_models gate"
```

---

## Task 2: Client-only shapes + isolated handling (types, redaction, new-overlay surfaces)

Adds the new `Intent`/`Action`/`Overlay` shapes and everything that can be tested *without* the Enter/Tab flow: the two `Action` handlers (fold a fetched list / a failure into the in-flight overlay), the `AddModelProviderKey`/`AddModelPick` submit arms, the `AddModelId` `api_key` branch, the `edit`/`nav` arms, `input_mode()` classification, and the render arms for the three new overlays. `pending_provider` stays; the Enter/Tab flow is unchanged (still opens `AddModelId` on Tab, still stages on Enter). One `unreachable!` arm is added in the cli so the workspace keeps compiling once the new intent exists.

**Files:**
- Modify: `crates/tui/src/action.rs` (`Intent` ~296-302 region; `Action` ~181 region; tests ~304-332)
- Modify: `crates/tui/src/state.rs` (`Overlay` ~164-185; `input_mode()` ~977-1010; add `filter_model_names` near `filter_models` ~704)
- Modify: `crates/tui/src/reduce.rs` (`reduce` match ~194; `submit_prompt` ~1035-1230; `edit_prompt` ~955-984; `nav` ~604-680; `AddModelId` literal in `begin_add_model` ~1251; imports ~16-20; add handler fns)
- Modify: `crates/tui/src/render.rs` (`render_overlays` ~948-1010; add `render_querying` + `render_add_model_pick`; imports ~24)
- Modify: `crates/cli/src/tui.rs` (`intent_to_command` ~911 — add the `QueryProviderModels` `unreachable!` arm)

**Interfaces:**
- Consumes: `SecretKey` (redacted `Debug`), `ProviderCard.can_list_models` (Task 1), `filter_models` shape.
- Produces:
  - `Intent::QueryProviderModels { provider_id: String, api_key: Option<SecretKey> }`
  - `Action::ProviderModelsLoaded { provider_id: String, models: Vec<String> }`
  - `Action::ProviderModelsFailed { provider_id: String, reason: String }`
  - `Overlay::AddModelProviderKey { provider_id: String, buffer: SecretKey }`
  - `Overlay::AddModelQuerying { provider_id: String, api_key: Option<SecretKey> }`
  - `Overlay::AddModelPick { provider_id: String, api_key: Option<SecretKey>, models: Vec<String>, query: String, selected: usize }`
  - `Overlay::AddModelId { provider_id: String, requires_key: bool, api_key: Option<SecretKey>, buffer: String }` (gains `api_key`)
  - `pub(crate) fn filter_model_names(models: &[String], query: &str) -> Vec<usize>`

- [ ] **Step 1: Add the `Intent` + `Action` variants**

In `crates/tui/src/action.rs`, inside `pub enum Intent` after the `AddModel { ... }` variant (~296-301, before the closing `}` of the enum ~302) add:

```rust
    /// Query a provider's OpenAI-compatible model list (client-only — NOT a
    /// daemon command). The harness GETs `<base_url>/models` with the provider's
    /// auth header and feeds the result back as `Action::ProviderModelsLoaded` /
    /// `ProviderModelsFailed`. `api_key` is the key the user entered for a hosted
    /// provider (redacted in `Debug`), or `None` for a local/no-auth provider.
    /// Intercepted in the harness drain loop, mirroring `AddModel`; never mapped
    /// to a `CommandBody`.
    QueryProviderModels {
        provider_id: String,
        api_key: Option<SecretKey>,
    },
```

In `pub enum Action`, after the `WorkflowNodeUpdated { ... }` variant (~149-160) and before `OpenPalette` (~163), add:

```rust
    /// A provider's model list, fetched by the harness (client-only add-model
    /// flow). Folds into the in-flight `Overlay::AddModelQuerying` (matched by
    /// `provider_id`) → `Overlay::AddModelPick`. Carries NO key — the key stays
    /// in the reducer's `AddModelQuerying` overlay across the round trip.
    ProviderModelsLoaded {
        provider_id: String,
        models: Vec<String>,
    },
    /// The model-list query failed (unreachable / non-200 / unparseable / auth
    /// rejected / empty). `reason` is a human, key-free message. Folds the
    /// in-flight query into the free-text `Overlay::AddModelId` fallback (carrying
    /// any already-entered key).
    ProviderModelsFailed {
        provider_id: String,
        reason: String,
    },
```

- [ ] **Step 2: Add the redaction test for `QueryProviderModels`**

In `crates/tui/src/action.rs`, in `#[cfg(test)] mod tests`, after `add_model_intent_debug_redacts_the_key` (~320-331) add:

```rust
    #[test]
    fn query_provider_models_intent_debug_redacts_the_key() {
        let intent = Intent::QueryProviderModels {
            provider_id: "groq".to_string(),
            api_key: Some(SecretKey("sk-secret".to_string())),
        };
        assert!(
            !format!("{intent:?}").contains("sk-secret"),
            "the key must never leak through the intent's Debug"
        );
    }
```

- [ ] **Step 3: Run the redaction test — verify pass (types compile in the tui crate)**

Run: `cargo test -p codypendent-tui query_provider_models_intent_debug_redacts_the_key 2>&1 | tail -20`
Expected: The tui crate compiles and the test PASSES. (The cli crate is not built by this `-p` invocation, so its `intent_to_command` break is not yet surfaced — fixed in Step 11.)

- [ ] **Step 4: Add `filter_model_names` to `state.rs`**

In `crates/tui/src/state.rs`, immediately after `filter_models` (~704-716) add:

```rust
/// The indices into `models` whose name case-insensitively contains `query` —
/// the add-model pick-list's substring filter, in list order. Mirrors
/// [`filter_models`] adapted to plain `String` model names (the provider's
/// `/models` ids are bare strings, not [`ModelCard`]s). An empty query matches
/// every name.
#[must_use]
pub(crate) fn filter_model_names(models: &[String], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    models
        .iter()
        .enumerate()
        .filter(|(_, name)| needle.is_empty() || name.to_lowercase().contains(&needle))
        .map(|(idx, _)| idx)
        .collect()
}
```

- [ ] **Step 5: Add the three new overlays + `AddModelId.api_key`**

In `crates/tui/src/state.rs`, in `pub enum Overlay`: first change the existing `AddModelId` variant (~170-174) to add the `api_key` field:

```rust
    /// Add-model flow, free-text fallback (step 2): the provider-side model name,
    /// for the catalog provider chosen in step 1 (`provider_id`). `requires_key`
    /// was read from that provider's card. `api_key`:
    ///   `None`    = no key captured yet → today's rule (`requires_key` ? advance
    ///               to [`Overlay::AddModelKey`] : emit `Intent::AddModel` with `None`).
    ///   `Some(k)` = key already captured (a can-list provider's failed query fell
    ///               back here, possibly blank) → emit `AddModel` directly with `k`
    ///               (blank normalized to `None`), no re-prompt.
    /// A blank name is rejected (the prompt stays open).
    AddModelId {
        provider_id: String,
        requires_key: bool,
        api_key: Option<SecretKey>,
        buffer: String,
    },
```

Then, after the existing `AddModelKey { ... }` variant (~180-184) and before the enum's closing `}` (~185) add the three new overlays:

```rust
    /// Add-model flow, key-first masked prompt (hosted, can-list only), shown
    /// BEFORE the query. `buffer` is the redacting [`SecretKey`] newtype (masked
    /// in render). On submit: emit `Intent::QueryProviderModels { provider_id,
    /// api_key }` and open [`Overlay::AddModelQuerying`].
    AddModelProviderKey {
        provider_id: String,
        buffer: SecretKey,
    },
    /// Add-model flow, transient "Fetching models from <provider>…" state while
    /// the harness GETs. Holds `api_key` across the round trip so the fed-back
    /// `Action` need not carry it. Non-interactive except `Esc` (cancels; a late
    /// result is ignored via the `provider_id`/overlay match guard).
    AddModelQuerying {
        provider_id: String,
        api_key: Option<SecretKey>,
    },
    /// Add-model flow, the model pick-list — fuzzy-filterable like the
    /// model/provider pickers. `Enter` on a row → `Intent::AddModel { display_id:
    /// "<provider>/<picked>", provider_id, model: <picked>, api_key }`; `Esc`
    /// closes. `query` filters `models` by substring; `selected` indexes the
    /// filtered results (reset to 0 when the query changes).
    AddModelPick {
        provider_id: String,
        api_key: Option<SecretKey>,
        models: Vec<String>,
        query: String,
        selected: usize,
    },
```

- [ ] **Step 6: Classify the new overlays in `input_mode()`**

In `crates/tui/src/state.rs`, in `input_mode()` (~977-1010):

Add `AddModelProviderKey` to the `Editing` group (it masks text like `AddModelKey`):

```rust
            Overlay::NewRun(_)
            | Overlay::Steering(_)
            | Overlay::DocEdit { .. }
            | Overlay::AddModelId { .. }
            | Overlay::AddModelKey { .. }
            | Overlay::AddModelProviderKey { .. } => InputMode::Editing,
```

Add `AddModelPick` to the `Palette` group (filter + navigate):

```rust
            Overlay::Palette { .. }
            | Overlay::ModelPicker { .. }
            | Overlay::ProviderPicker { .. }
            | Overlay::AddModelPick { .. } => InputMode::Palette,
```

Add `AddModelQuerying` to the `Normal` group (non-interactive; `Esc` dismisses):

```rust
            Overlay::Help
            | Overlay::Skills
            | Overlay::Memory { .. }
            | Overlay::Docs
            | Overlay::Edges
            | Overlay::Workflow
            | Overlay::Blackboard
            | Overlay::AddModelQuerying { .. } => InputMode::Normal,
```

- [ ] **Step 7: Write failing reducer tests for the two `Action` handlers + the new submit arms**

In `crates/tui/src/reduce.rs`, in `#[cfg(test)] mod tests`, after `add_model_escape_abandons_the_flow_without_emitting` (~3932) add:

```rust
    // --- model discovery: Action handlers (isolated, no Enter/Tab flow) ---

    #[test]
    fn provider_models_loaded_opens_the_pick_list_carrying_the_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: Some(SecretKey("sk-secret".to_owned())),
        };
        reduce(
            &mut s,
            Action::ProviderModelsLoaded {
                provider_id: "groq".to_owned(),
                models: vec!["llama-3.1-8b".to_owned(), "llama-3.3-70b".to_owned()],
            },
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelPick {
                provider_id: "groq".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
                models: vec!["llama-3.1-8b".to_owned(), "llama-3.3-70b".to_owned()],
                query: String::new(),
                selected: 0,
            }
        );
    }

    #[test]
    fn provider_models_loaded_for_a_mismatched_provider_is_ignored() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: None,
        };
        reduce(
            &mut s,
            Action::ProviderModelsLoaded {
                provider_id: "ollama".to_owned(),
                models: vec!["qwen".to_owned()],
            },
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "groq".to_owned(),
                api_key: None,
            },
            "a stale result for another provider must not replace the overlay"
        );
    }

    #[test]
    fn provider_models_failed_falls_back_to_free_text_carrying_the_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: Some(SecretKey("sk-secret".to_owned())),
        };
        reduce(
            &mut s,
            Action::ProviderModelsFailed {
                provider_id: "groq".to_owned(),
                reason: "HTTP 401".to_owned(),
            },
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelId {
                provider_id: "groq".to_owned(),
                requires_key: true,
                api_key: Some(SecretKey("sk-secret".to_owned())),
                buffer: String::new(),
            }
        );
        let notice = s.notice.as_ref().expect("a fallback notice").0.clone();
        assert!(notice.contains("HTTP 401"), "the notice explains why: {notice}");
    }

    #[test]
    fn provider_models_failed_for_a_local_provider_falls_back_with_no_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "ollama".to_owned(),
            api_key: None,
        };
        reduce(
            &mut s,
            Action::ProviderModelsFailed {
                provider_id: "ollama".to_owned(),
                reason: "could not connect to the provider".to_owned(),
            },
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelId {
                provider_id: "ollama".to_owned(),
                requires_key: false,
                api_key: None,
                buffer: String::new(),
            }
        );
    }

    #[test]
    fn provider_models_failed_for_a_mismatched_provider_is_ignored() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: None,
        };
        reduce(
            &mut s,
            Action::ProviderModelsFailed {
                provider_id: "ollama".to_owned(),
                reason: "x".to_owned(),
            },
        );
        assert!(matches!(s.overlay, Overlay::AddModelQuerying { .. }));
    }

    // --- model discovery: new overlay submit arms (isolated) ---

    #[test]
    fn add_model_provider_key_submit_queries_with_the_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelProviderKey {
            provider_id: "groq".to_owned(),
            buffer: SecretKey("sk-secret".to_owned()),
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.outbox,
            vec![Intent::QueryProviderModels {
                provider_id: "groq".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }]
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "groq".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }
        );
    }

    #[test]
    fn add_model_provider_key_blank_queries_with_no_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelProviderKey {
            provider_id: "groq".to_owned(),
            buffer: SecretKey(String::new()),
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.outbox,
            vec![Intent::QueryProviderModels {
                provider_id: "groq".to_owned(),
                api_key: None,
            }]
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "groq".to_owned(),
                api_key: None,
            }
        );
    }

    #[test]
    fn add_model_pick_submit_emits_add_model_with_the_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelPick {
            provider_id: "groq".to_owned(),
            api_key: Some(SecretKey("sk-secret".to_owned())),
            models: vec!["llama-3.1-8b".to_owned(), "llama-3.3-70b".to_owned()],
            query: String::new(),
            selected: 1,
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.overlay, Overlay::None);
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "groq/llama-3.3-70b".to_owned(),
                provider_id: "groq".to_owned(),
                model: "llama-3.3-70b".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }]
        );
    }

    #[test]
    fn add_model_pick_zero_match_emits_nothing() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelPick {
            provider_id: "groq".to_owned(),
            api_key: None,
            models: vec!["llama-3.1-8b".to_owned()],
            query: "zzz-nope".to_owned(),
            selected: 0,
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.overlay, Overlay::None, "the picker still closes");
        assert!(s.outbox.is_empty(), "a zero-match submit adds nothing");
    }

    #[test]
    fn add_model_pick_filters_and_navigates() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelPick {
            provider_id: "groq".to_owned(),
            api_key: None,
            models: vec!["llama-3.1-8b".to_owned(), "gpt-oss-20b".to_owned()],
            query: String::new(),
            selected: 1,
        };
        // Typing resets the selection to the top of the new filtered set.
        for c in "gpt".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        match &s.overlay {
            Overlay::AddModelPick { query, selected, .. } => {
                assert_eq!(query, "gpt");
                assert_eq!(*selected, 0);
            }
            other => panic!("expected the pick-list, got {other:?}"),
        }
        // Down clamps at the single filtered row.
        reduce(&mut s, Action::SelectNext);
        match &s.overlay {
            Overlay::AddModelPick { selected, .. } => assert_eq!(*selected, 0),
            other => panic!("expected the pick-list, got {other:?}"),
        }
    }

    #[test]
    fn add_model_id_with_a_captured_key_emits_directly_without_re_prompting() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelId {
            provider_id: "groq".to_owned(),
            requires_key: true,
            api_key: Some(SecretKey("sk-secret".to_owned())),
            buffer: "llama-3.1-8b".to_owned(),
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.overlay, Overlay::None, "no AddModelKey step — key already held");
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "groq/llama-3.1-8b".to_owned(),
                provider_id: "groq".to_owned(),
                model: "llama-3.1-8b".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }]
        );
    }
```

- [ ] **Step 8: Run the new reducer tests — verify they fail**

Run: `cargo test -p codypendent-tui provider_models_ add_model_provider_key add_model_pick add_model_id_with_a_captured 2>&1 | tail -30`
Expected: FAIL — compile errors / behavior mismatches (handlers and arms not written yet; the existing `AddModelId` submit arm ignores `api_key`).

- [ ] **Step 9: Implement the reducer handlers, arms, and imports**

In `crates/tui/src/reduce.rs`:

(a) Extend the state import (~16-20) to add `filter_model_names`:

```rust
use crate::state::{
    filter_model_names, filter_models, filter_providers, AppState, DocBlockView, DocEdit, DocFocus,
    DocLeaseState, DocSuggestionView, Overlay, Pane, PatchSummary, PendingApproval, RunActivity,
    RunView, ToolCard, ToolStatus, TranscriptEntry,
};
```

(b) In the `reduce` match, add two arms just before `Action::NoOp => {}` (~194):

```rust
        Action::ProviderModelsLoaded {
            provider_id,
            models,
        } => on_provider_models_loaded(state, provider_id, models),
        Action::ProviderModelsFailed {
            provider_id,
            reason,
        } => on_provider_models_failed(state, provider_id, reason),
```

(c) Add the two handler functions (e.g. immediately after `apply_workflow_node_update`, or anywhere at module scope — place after `begin_add_model` ~1256 for locality):

```rust
/// Fold a fetched provider model list into the in-flight query overlay
/// (model-discovery). Moves the stashed `api_key` from `AddModelQuerying` into
/// the pick-list so the round-trip `Action` never carries the key. If the
/// overlay is no longer the matching `AddModelQuerying` (the user dismissed or
/// opened something else, or this is a stale result for another provider), the
/// result is ignored — the race guard.
fn on_provider_models_loaded(state: &mut AppState, provider_id: String, models: Vec<String>) {
    let matched = matches!(
        &state.overlay,
        Overlay::AddModelQuerying { provider_id: pid, .. } if *pid == provider_id
    );
    if !matched {
        return;
    }
    if let Overlay::AddModelQuerying {
        provider_id: pid,
        api_key,
    } = std::mem::replace(&mut state.overlay, Overlay::None)
    {
        state.overlay = Overlay::AddModelPick {
            provider_id: pid,
            api_key,
            models,
            query: String::new(),
            selected: 0,
        };
    }
}

/// Fold a failed model-list query into the free-text fallback (model-discovery):
/// move the stashed `api_key` from `AddModelQuerying` into `AddModelId` so a
/// hosted provider is never asked for its key twice, and surface a key-free
/// notice. Ignored (race guard) if the overlay no longer matches.
fn on_provider_models_failed(state: &mut AppState, provider_id: String, reason: String) {
    let matched = matches!(
        &state.overlay,
        Overlay::AddModelQuerying { provider_id: pid, .. } if *pid == provider_id
    );
    if !matched {
        return;
    }
    if let Overlay::AddModelQuerying {
        provider_id: pid,
        api_key,
    } = std::mem::replace(&mut state.overlay, Overlay::None)
    {
        let requires_key = api_key.is_some();
        state.notice = Some((
            format!("couldn't fetch models ({reason}); type the model name"),
            state.tick + 25,
        ));
        state.overlay = Overlay::AddModelId {
            provider_id: pid,
            requires_key,
            api_key,
            buffer: String::new(),
        };
    }
}
```

(d) In `submit_prompt`, replace the existing `Overlay::AddModelId { provider_id, requires_key, buffer } => { ... }` arm (~1176-1205) with the `api_key`-aware version:

```rust
        // Add-model free-text fallback: a captured key emits directly; otherwise
        // today's rule (hosted → masked key prompt; local → emit now). A blank
        // name reopens the prompt, carrying any captured key. `mem::take` left
        // the overlay `None`.
        Overlay::AddModelId {
            provider_id,
            requires_key,
            api_key,
            buffer,
        } => {
            let model = buffer.trim().to_owned();
            if model.is_empty() {
                state.notice = Some(("model name cannot be blank".to_owned(), state.tick + 25));
                state.overlay = Overlay::AddModelId {
                    provider_id,
                    requires_key,
                    api_key,
                    buffer: String::new(),
                };
            } else if let Some(key) = api_key {
                // A key was already captured (a can-list provider's failed query
                // fell back here). Emit directly — never re-prompt. A blank inner
                // key normalizes to `None`.
                let display_id = format!("{provider_id}/{model}");
                let inner = key.0.trim().to_owned();
                let api_key = if inner.is_empty() {
                    None
                } else {
                    Some(SecretKey(inner))
                };
                state.notice = Some((format!("adding model {display_id}"), state.tick + 25));
                state.outbox.push(Intent::AddModel {
                    display_id,
                    provider_id,
                    model,
                    api_key,
                });
            } else if requires_key {
                state.overlay = Overlay::AddModelKey {
                    provider_id,
                    model,
                    buffer: SecretKey(String::new()),
                };
            } else {
                let display_id = format!("{provider_id}/{model}");
                state.notice = Some((format!("adding model {display_id}"), state.tick + 25));
                state.outbox.push(Intent::AddModel {
                    display_id,
                    provider_id,
                    model,
                    api_key: None,
                });
            }
        }
```

(e) In `submit_prompt`, add two new arms just before the final `other => state.overlay = other,` (~1229):

```rust
        // Key-first prompt (can-list hosted): emit the query with the entered key
        // (blank → no key) and open the transient "Fetching…" state, keeping the
        // key in the overlay for the round trip.
        Overlay::AddModelProviderKey { provider_id, buffer } => {
            let key = buffer.0.trim().to_owned();
            let api_key = if key.is_empty() {
                None
            } else {
                Some(SecretKey(key))
            };
            state.outbox.push(Intent::QueryProviderModels {
                provider_id: provider_id.clone(),
                api_key: api_key.clone(),
            });
            state.overlay = Overlay::AddModelQuerying {
                provider_id,
                api_key,
            };
        }
        // The pick-list: resolve the filtered selection (same zero-match guard as
        // the model picker) and emit `AddModel` for the chosen name, moving the
        // stashed key into the intent.
        Overlay::AddModelPick {
            provider_id,
            api_key,
            models,
            query,
            selected,
        } => {
            if let Some(&idx) = filter_model_names(&models, &query).get(selected) {
                if let Some(model) = models.get(idx) {
                    let model = model.clone();
                    let display_id = format!("{provider_id}/{model}");
                    state.notice = Some((format!("adding model {display_id}"), state.tick + 25));
                    state.outbox.push(Intent::AddModel {
                        display_id,
                        provider_id,
                        model,
                        api_key,
                    });
                }
            }
        }
```

(f) In `edit_prompt`, add two arms among the overlay arms (after `Overlay::AddModelKey { buffer, .. } => edit(&mut buffer.0),` ~961):

```rust
        // The key-first prompt masks a redacting newtype, like `AddModelKey`.
        Overlay::AddModelProviderKey { buffer, .. } => edit(&mut buffer.0),
        // The pick-list filters like the model picker: editing the query resets
        // the selection to the top of the new filtered set.
        Overlay::AddModelPick { query, selected, .. } => {
            edit(query);
            *selected = 0;
        }
```

(g) In `nav`, add an arm mirroring the `ModelPicker` arm, before the final `_ => {}` (~679). Note `models` is the overlay's own field:

```rust
        Overlay::AddModelPick {
            ref query,
            ref mut selected,
            ref models,
            ..
        } => {
            let indices = filter_model_names(models, query);
            step(selected, indices.len(), delta);
            return;
        }
```

(h) In `begin_add_model` (~1251), update the `AddModelId` construction to include the new field (behavior unchanged — Tab still opens the free-text prompt this task):

```rust
    state.overlay = Overlay::AddModelId {
        provider_id,
        requires_key,
        api_key: None,
        buffer: String::new(),
    };
```

- [ ] **Step 10: Fix the two existing `AddModelId` test literals so the tui crate compiles**

In `crates/tui/src/reduce.rs`, the Tab test's expected `AddModelId` (~3807-3811) gains `api_key: None`:

```rust
            Overlay::AddModelId {
                provider_id: "groq".to_owned(),
                requires_key: true,
                api_key: None,
                buffer: String::new(),
            }
```

(The local-provider add-model test at ~3872 matches `AddModelId { requires_key: false, .. }`, which already tolerates the new field.) These tests are rewritten in Task 4; this step only keeps them compiling.

- [ ] **Step 11: Add the `QueryProviderModels` `unreachable!` arm in the cli**

In `crates/cli/src/tui.rs`, in `intent_to_command` (~908-913), replace the single `Intent::AddModel { .. } => unreachable!(...)` arm with both client-only arms:

```rust
        // `AddModel` and `QueryProviderModels` are CLIENT-ONLY intents applied
        // locally by the harness (see the drain loop's interceptions); neither
        // becomes a daemon command, so these mappings are never reached.
        Intent::AddModel { .. } => unreachable!(
            "AddModel is applied locally by the harness (write_add_model), never sent to the daemon"
        ),
        Intent::QueryProviderModels { .. } => unreachable!(
            "QueryProviderModels is applied locally by the harness (background GET), never sent to the daemon"
        ),
```

- [ ] **Step 12: Add render arms for the three new overlays + the render helpers**

In `crates/tui/src/render.rs`:

(a) Extend the state import (~24-25) to add `filter_model_names`:

```rust
use crate::state::{
    filter_model_names, filter_models, filter_providers, AppState, DocFocus, DocLeaseState,
    LayoutMode, ModelCard,
```

_(Keep the remainder of the existing `use crate::state::{ ... }` list unchanged after `ModelCard`.)_

(b) In `render_overlays`, add three arms just before `Overlay::None => { ... }` (~1005):

```rust
        Overlay::AddModelProviderKey { provider_id, buffer } => {
            render_masked_prompt(
                frame,
                area,
                theme,
                &format!("API key for {provider_id} (used to list its models; stored locally 0600)"),
                &buffer.0,
            );
        }
        Overlay::AddModelQuerying { provider_id, .. } => {
            render_querying(frame, area, theme, provider_id);
        }
        Overlay::AddModelPick {
            provider_id,
            models,
            query,
            selected,
            ..
        } => {
            render_add_model_pick(frame, area, theme, provider_id, models, query, *selected);
        }
```

(c) Add the two render helpers (e.g. after `render_add_model_pick`'s sibling `render_provider_picker`, ~1557, or after `render_masked_prompt` ~2645). `render_querying` (static box — the loop's own tick redraws animate it):

```rust
/// The transient "Fetching models from <provider>…" box shown while the harness
/// GETs the provider's `/models` list (model-discovery). Non-interactive except
/// `Esc`, which cancels the wait. Colors are Theme tokens only (RULE 7). The key
/// is NOT in scope here (the overlay's `api_key` field is dropped via `..`).
fn render_querying(frame: &mut Frame, area: Rect, theme: &Theme, provider_id: &str) {
    let rect = centered_rect(70, 20, area);
    frame.render_widget(Clear, rect);
    let lines = vec![
        Line::styled(
            format!("Fetching models from {provider_id}…"),
            Style::default().fg(theme.text.heading),
        ),
        Line::raw(""),
        Line::styled("Esc to cancel", Style::default().fg(theme.text.muted)),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        rect,
    );
}

/// The add-model pick-list (model-discovery): a filter line over the provider's
/// fetched model ids, the same shape as [`render_model_picker`] but over plain
/// `String` names (there is no `ModelCard` detail to show). Colors are Theme
/// tokens only (RULE 7). The key is NOT in scope here.
fn render_add_model_pick(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    provider_id: &str,
    models: &[String],
    query: &str,
    selected: usize,
) {
    let rect = centered_rect(84, 84, area);
    frame.render_widget(Clear, rect);

    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Add a model from {provider_id} ({}) ", models.len()),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let filter = Line::from(vec![
        Span::styled("› ", Style::default().fg(theme.focus.active)),
        Span::styled(query.to_owned(), Style::default().fg(theme.text.primary)),
        Span::styled("▏", Style::default().fg(theme.focus.active)),
    ]);
    frame.render_widget(
        Paragraph::new(filter).style(Style::default().bg(theme.surface.overlay)),
        rows[0],
    );

    let matches = filter_model_names(models, query);
    let mut items: Vec<ListItem> = Vec::new();
    if models.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no models returned",
            Style::default().fg(theme.text.muted),
        )));
    } else if matches.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no matching model",
            Style::default().fg(theme.text.muted),
        )));
    }
    for (row, &idx) in matches.iter().enumerate() {
        let is_selected = row == selected;
        let head = Line::from(vec![
            Span::styled(
                if is_selected { "› " } else { "  " },
                Style::default().fg(theme.focus.active),
            ),
            Span::styled(
                truncate(&models[idx], 40),
                Style::default().fg(theme.text.primary),
            ),
        ]);
        let item = ListItem::new(vec![head]);
        items.push(if is_selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        rows[1],
    );
}
```

- [ ] **Step 13: Write render tests for the new overlays**

In `crates/tui/src/render.rs`, in the render `#[cfg(test)] mod tests`, after `masked_key_prompt_hides_the_typed_key` (~4426) add:

```rust
    #[test]
    fn add_model_provider_key_prompt_masks_the_key() {
        let mut state = AppState::new();
        state.overlay = Overlay::AddModelProviderKey {
            provider_id: "groq".to_owned(),
            buffer: crate::action::SecretKey("sk-secret".to_owned()),
        };
        let text = render_to_string(&state, 100, 24);
        assert!(text.contains("API key for groq"), "the key prompt title:\n{text}");
        assert!(text.contains('•'), "the key is masked with bullets:\n{text}");
        assert!(!text.contains("sk-secret"), "the raw key must never render:\n{text}");
    }

    #[test]
    fn add_model_querying_box_names_the_provider() {
        let mut state = AppState::new();
        state.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: Some(crate::action::SecretKey("sk-secret".to_owned())),
        };
        let text = render_to_string(&state, 80, 24);
        assert!(
            text.contains("Fetching models from groq"),
            "the querying box names the provider:\n{text}"
        );
        assert!(text.contains("Esc to cancel"), "the cancel hint:\n{text}");
        assert!(!text.contains("sk-secret"), "the key must never render:\n{text}");
    }

    #[test]
    fn add_model_pick_lists_and_filters_names() {
        let mut state = AppState::new();
        state.overlay = Overlay::AddModelPick {
            provider_id: "groq".to_owned(),
            api_key: None,
            models: vec!["llama-3.1-8b".to_owned(), "gpt-oss-20b".to_owned()],
            query: "llama".to_owned(),
            selected: 0,
        };
        let text = render_to_string(&state, 100, 30);
        assert!(text.contains("Add a model from groq"), "the pick-list title:\n{text}");
        assert!(text.contains("llama-3.1-8b"), "the matching model lists:\n{text}");
        assert!(!text.contains("gpt-oss-20b"), "a non-matching model is filtered out:\n{text}");
    }
```

- [ ] **Step 14: Run the whole tui + cli suites — verify pass**

Run: `cargo test -p codypendent-tui && cargo test -p codypendent-cli 2>&1 | tail -20`
Expected: PASS (new reducer + render tests green; existing tests still green; the cli compiles with the new `unreachable!` arm).

- [ ] **Step 15: Clippy + commit**

Run: `cargo clippy -p codypendent-tui -p codypendent-cli --all-targets -- -D warnings`
Expected: no warnings.

```bash
git add crates/tui/src/action.rs crates/tui/src/state.rs crates/tui/src/reduce.rs crates/tui/src/render.rs crates/cli/src/tui.rs
git commit -m "feat(tui): add QueryProviderModels/ProviderModels* shapes + add-model overlays"
```

---

## Task 3: Harness — `QueryProviderModels` interception + background `/models` GET

The only I/O. Adds the `reqwest` dep, the `ReaderSignal::ProviderModels` variant + its `select!` mapping, the drain-loop interception that resolves the catalog provider and spawns the GET, and the pure `models_url` / `parse_models_response` helpers plus the async `query_provider_models`. Sequenced before the flow (Task 4) so the interception exists before anything emits the intent. No reducer/UI change here.

**Files:**
- Modify: `crates/cli/Cargo.toml` (add `reqwest`)
- Modify: `crates/cli/src/tui.rs` (`run` ~195-196; `event_loop` signature ~458-476 + call ~218-235; `select!` arm ~575; drain loop ~614-668; `ReaderSignal` enum ~693-717; add `query_provider_models`/`models_url`/`parse_models_response`; tests)

**Interfaces:**
- Consumes: `Intent::QueryProviderModels` (Task 2), `Action::ProviderModelsLoaded`/`ProviderModelsFailed` (Task 2), `codypendent_providers::{Catalog, AuthMethod}`, `Provider.{base_url, auth}`.
- Produces:
  - `ReaderSignal::ProviderModels { provider_id: String, result: Result<Vec<String>, String> }`
  - `fn models_url(base_url: &str) -> String`
  - `fn parse_models_response(body: &str) -> Result<Vec<String>, String>`
  - `async fn query_provider_models(base_url: &str, header: &str, prefix: &str, api_key: Option<&str>) -> Result<Vec<String>, String>`
  - `event_loop` gains a `query_tx: mpsc::Sender<ReaderSignal>` parameter.

- [ ] **Step 1: Add `reqwest` to the cli crate**

In `crates/cli/Cargo.toml`, in `[dependencies]` after `serde_json = { workspace = true }` (~55) add:

```toml
# The workspace HTTP stack (rustls, no system OpenSSL), reused for the add-model
# flow's background `<base_url>/models` GET (model-discovery). The CLI does not
# depend on it directly yet; the same crate the GitHub client uses.
reqwest = { workspace = true }
```

- [ ] **Step 2: Write failing tests for `models_url` + `parse_models_response`**

In `crates/cli/src/tui.rs`, in `#[cfg(test)] mod tests`, after the `provider_can_list_models` tests (Task 1) add:

```rust
    // -- models_url + parse_models_response (model-discovery, pure) -----------

    #[test]
    fn models_url_appends_models_without_doubling_the_version() {
        // The base_url already carries its version segment; the list route is its
        // sibling `/models`, never `/v1/models`.
        assert_eq!(
            models_url("https://api.groq.com/openai/v1"),
            "https://api.groq.com/openai/v1/models"
        );
        assert_eq!(models_url("http://localhost:11434/v1"), "http://localhost:11434/v1/models");
        // A non-`/v1` base (z.ai) must not be forced to `/v1`.
        assert_eq!(
            models_url("https://api.z.ai/api/paas/v4"),
            "https://api.z.ai/api/paas/v4/models"
        );
        // A trailing slash is trimmed so the join is exact.
        assert_eq!(models_url("http://localhost:1234/v1/"), "http://localhost:1234/v1/models");
    }

    #[test]
    fn parse_models_response_extracts_ids_from_the_openai_shape() {
        let body = r#"{"object":"list","data":[{"id":"llama-3.1-8b"},{"id":"llama-3.3-70b"}]}"#;
        assert_eq!(
            parse_models_response(body).expect("parse"),
            vec!["llama-3.1-8b".to_string(), "llama-3.3-70b".to_string()]
        );
    }

    #[test]
    fn parse_models_response_skips_blank_and_dedups_preserving_order() {
        let body = r#"{"data":[{"id":"a"},{"id":"  "},{"id":""},{"id":"a"},{"id":"b"}]}"#;
        assert_eq!(
            parse_models_response(body).expect("parse"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn parse_models_response_errors_on_an_empty_list() {
        let body = r#"{"object":"list","data":[]}"#;
        let err = parse_models_response(body).expect_err("empty list must be an error");
        assert!(err.contains("no models"), "reason: {err}");
    }

    #[test]
    fn parse_models_response_errors_on_a_malformed_body() {
        assert!(parse_models_response("not json at all").is_err());
        assert!(parse_models_response("").is_err());
    }
```

- [ ] **Step 3: Run the tests — verify they fail**

Run: `cargo test -p codypendent-cli models_url parse_models_response 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'models_url'` / `'parse_models_response'`.

- [ ] **Step 4: Implement `models_url`, `parse_models_response`, and `query_provider_models`**

In `crates/cli/src/tui.rs`, add near `write_add_model` (e.g. after it, ~1038). These three functions carry the endpoint + parsing + I/O:

```rust
/// The provider's OpenAI-compatible model-list URL: `<base_url>/models`. The
/// catalog `base_url` already carries its version segment (`…/v1`, `…/v4`, …),
/// so the list route is its sibling `/models` — never `/v1/models` (which would
/// double the version). A trailing slash is trimmed so the join is exact.
fn models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

/// Parse an OpenAI/Ollama `/models` response body (`{ "object": "list", "data":
/// [ { "id": "…" }, … ] }`) into the model ids: trim each, skip blank/missing,
/// dedup preserving order. An empty result is an `Err` so the reducer's failure
/// arm routes to the free-text fallback uniformly. A pure function over the body
/// string — the network GET is in `query_provider_models` — so it is directly
/// unit-testable. The error strings are generic and never carry a key.
fn parse_models_response(body: &str) -> Result<Vec<String>, String> {
    #[derive(serde::Deserialize)]
    struct ModelsResponse {
        #[serde(default)]
        data: Vec<ModelEntry>,
    }
    #[derive(serde::Deserialize)]
    struct ModelEntry {
        #[serde(default)]
        id: String,
    }
    let parsed: ModelsResponse = serde_json::from_str(body)
        .map_err(|_| "the provider returned an unexpected response".to_string())?;
    let mut ids: Vec<String> = Vec::new();
    for entry in parsed.data {
        let id = entry.id.trim().to_string();
        if id.is_empty() || ids.contains(&id) {
            continue;
        }
        ids.push(id);
    }
    if ids.is_empty() {
        return Err("provider returned no models".to_string());
    }
    Ok(ids)
}

/// GET `<base_url>/models` for the add-model flow (model-discovery), applying the
/// provider's auth header only when a non-blank `api_key` is present (a keyless
/// OpenAI-compatible endpoint sends none). Bounded at 10s so a hung endpoint
/// can't wedge the query task. Non-2xx → `Err` with the STATUS ONLY (never the
/// key); the body is parsed defensively. Every returned `reason` is key-free and
/// URL-free (send errors map to fixed strings; the auth value is marked
/// sensitive so reqwest cannot echo it in any error). This is the only I/O in
/// the model-discovery feature; it runs on a spawned task off the UI thread.
async fn query_provider_models(
    base_url: &str,
    header: &str,
    prefix: &str,
    api_key: Option<&str>,
) -> Result<Vec<String>, String> {
    let url = models_url(base_url);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "could not build the HTTP client".to_string())?;
    let mut request = client.get(&url);
    if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        // Mark the auth value sensitive so reqwest redacts it from any error /
        // debug (mirrors the GitHub client). The key never appears in a reason.
        match reqwest::header::HeaderValue::from_str(&format!("{prefix}{key}")) {
            Ok(mut value) => {
                value.set_sensitive(true);
                request = request.header(header, value);
            }
            Err(_) => return Err("the API key is not a valid header value".to_string()),
        }
    }
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "request timed out".to_string()
        } else if error.is_connect() {
            "could not connect to the provider".to_string()
        } else {
            "the model-list request failed".to_string()
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let body = response
        .text()
        .await
        .map_err(|_| "could not read the response body".to_string())?;
    parse_models_response(&body)
}
```

- [ ] **Step 5: Run the pure tests — verify pass**

Run: `cargo test -p codypendent-cli models_url parse_models_response 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Add the `ReaderSignal::ProviderModels` variant**

In `crates/cli/src/tui.rs`, in `enum ReaderSignal` (~693-717), before `/// The daemon closed the connection.` / `Closed,` add:

```rust
    /// A provider's fetched model list (model-discovery): the result of the
    /// spawned `<base_url>/models` GET, keyed by `provider_id`. Mapped by the
    /// loop's `select!` to `Action::ProviderModelsLoaded` (Ok) /
    /// `ProviderModelsFailed` (Err). Carries NO key.
    ProviderModels {
        provider_id: String,
        result: Result<Vec<String>, String>,
    },
```

- [ ] **Step 7: Map the signal to an `Action` in the loop's `select!`**

In `crates/cli/src/tui.rs`, in `event_loop`'s `tokio::select!` (~506-576), before the `Some(ReaderSignal::Closed) | None => return Ok(())` arm (~575) add:

```rust
                Some(ReaderSignal::ProviderModels { provider_id, result }) => match result {
                    Ok(models) => Action::ProviderModelsLoaded { provider_id, models },
                    Err(reason) => Action::ProviderModelsFailed { provider_id, reason },
                },
```

- [ ] **Step 8: Thread `query_tx` into `event_loop`**

In `crates/cli/src/tui.rs`, in `run`, replace the reader-channel setup (~195-196):

```rust
    let (event_tx, mut event_rx) = mpsc::channel::<ReaderSignal>(256);
    // A second sender clone for the model-discovery query tasks, which feed
    // `ReaderSignal::ProviderModels` back into the same loop (the reader task
    // owns the first clone).
    let query_tx = event_tx.clone();
    let reader = tokio::spawn(read_loop(read_half, event_tx, out_tx.clone(), client_id));
```

Add the argument to the `event_loop(...)` call (~218-235) — insert `query_tx` after `&mut event_rx`:

```rust
        &mut event_rx,
        query_tx,
        &mut input_rx,
```

Add the parameter to the `event_loop` signature (~458-476) — after `event_rx: &mut mpsc::Receiver<ReaderSignal>,` (~463):

```rust
    event_rx: &mut mpsc::Receiver<ReaderSignal>,
    query_tx: mpsc::Sender<ReaderSignal>,
    input_rx: &mut mpsc::Receiver<CrosstermEvent>,
```

- [ ] **Step 9: Intercept `Intent::QueryProviderModels` in the drain loop and spawn the GET**

In `crates/cli/src/tui.rs`, in the `for intent in state.drain_outbox()` loop (~614), immediately after the `Intent::AddModel { ... } { ...; continue; }` block (~617-639) add:

```rust
            // `QueryProviderModels` is the other client-only intent (model
            // discovery): resolve the catalog provider's base_url + first
            // api-key header/prefix, then spawn the `<base_url>/models` GET off
            // the UI thread and feed the result back as
            // `ReaderSignal::ProviderModels`. Never a daemon command. The spawned
            // task owns the key for the request and drops it — it is never sent
            // back.
            if let Intent::QueryProviderModels {
                provider_id,
                api_key,
            } = &intent
            {
                use codypendent_providers::{AuthMethod, Catalog};
                let catalog =
                    Catalog::load_with_user_overrides(&paths.data_dir.join("providers.toml"))
                        .unwrap_or_else(|_| Catalog::builtin());
                let (base_url, header, prefix) = match catalog.get(provider_id) {
                    Some(provider) => {
                        let base = provider.base_url.clone().unwrap_or_default();
                        let (header, prefix) = match provider.auth.first() {
                            Some(AuthMethod::ApiKey { header, prefix, .. }) => {
                                (header.clone(), prefix.clone())
                            }
                            _ => ("Authorization".to_string(), "Bearer ".to_string()),
                        };
                        (base, header, prefix)
                    }
                    None => (
                        String::new(),
                        "Authorization".to_string(),
                        "Bearer ".to_string(),
                    ),
                };
                let provider_id = provider_id.clone();
                let key = api_key.as_ref().map(|k| k.0.clone());
                let tx = query_tx.clone();
                tokio::spawn(async move {
                    let result =
                        query_provider_models(&base_url, &header, &prefix, key.as_deref()).await;
                    let _ = tx
                        .send(ReaderSignal::ProviderModels {
                            provider_id,
                            result,
                        })
                        .await;
                });
                continue;
            }
```

- [ ] **Step 10: Build + run the cli suite — verify pass**

Run: `cargo test -p codypendent-cli 2>&1 | tail -20`
Expected: PASS (the crate compiles with the new `query_tx` param, the `select!` arm, and the interception; the pure tests are green).

- [ ] **Step 11: Clippy + commit**

Run: `cargo clippy -p codypendent-cli --all-targets -- -D warnings`
Expected: no warnings.

```bash
git add crates/cli/Cargo.toml crates/cli/src/tui.rs Cargo.lock
git commit -m "feat(cli): harness GET for QueryProviderModels via ReaderSignal::ProviderModels"
```

_(If `Cargo.lock` is unchanged by the reqwest addition — it was already resolved for the workspace — just omit it from the `git add`.)_

---

## Task 4: Reducer flow — `Enter`/`Tab` → `enter_add_model_flow`; remove `pending_provider`

Wires the picker to the new flow and deletes the dead staging. `Enter` and `Tab` both begin the add-model flow through one shared helper that branches on `can_list_models` / `requires_key`. The `pending_provider` field and its false "applies to your next run" notice are removed (state + reducer + the render staged-marker that referenced the field). After this task the full loop is live: pick a can-list provider → key/query → pick-list → add; pick a cannot-list provider → free-text.

**Files:**
- Modify: `crates/tui/src/reduce.rs` (`begin_add_model` ~1238-1256; `ProviderPicker` Enter arm ~1117-1128; add `enter_add_model_flow`; rewrite the four affected add-model tests + the three `pending_provider` tests ~3699-3932)
- Modify: `crates/tui/src/state.rs` (remove `pending_provider` field ~875, `new()` init ~959; update the `ProviderPicker` doc ~160-163)
- Modify: `crates/tui/src/render.rs` (remove the `state.pending_provider` staged-marker in `render_provider_picker` ~1429-1516; rewrite the render test that set `pending_provider` ~4334-4406)
- Modify: `crates/tui/src/action.rs` (update the `BeginAddModel` doc ~164-167)

**Interfaces:**
- Consumes: `ProviderCard.{requires_key, can_list_models}` (Task 1), the overlays + `Intent::QueryProviderModels` (Task 2), the harness interception (Task 3).
- Produces: `fn enter_add_model_flow(state: &mut AppState, provider_id: String, requires_key: bool, can_list_models: bool)`; `pending_provider` removed everywhere.

- [ ] **Step 1: Write the failing flow tests**

In `crates/tui/src/reduce.rs`, add new flow tests (place near the existing add-model tests, e.g. after `add_model_escape_abandons_the_flow_without_emitting` ~3932, alongside the Task 2 tests):

```rust
    // --- model discovery: Enter/Tab begin the add-model flow ---

    #[test]
    fn provider_picker_enter_can_list_hosted_opens_the_key_prompt() {
        let mut s = AppState::new();
        // groq: openai-chat + api-key → can_list + requires_key.
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        open_provider_picker(&mut s); // focuses groq
        reduce(&mut s, Action::InputSubmit); // Enter begins the flow
        assert_eq!(
            s.overlay,
            Overlay::AddModelProviderKey {
                provider_id: "groq".to_owned(),
                buffer: SecretKey(String::new()),
            }
        );
        assert!(s.outbox.is_empty(), "no query until the key is entered");
    }

    #[test]
    fn provider_picker_enter_can_list_local_queries_immediately() {
        let mut s = AppState::new();
        // ollama: openai-chat + none → can_list, no key.
        s.providers = vec![provider_card("ollama", "Ollama (local)", "openai-chat", "none", true)];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.outbox,
            vec![Intent::QueryProviderModels {
                provider_id: "ollama".to_owned(),
                api_key: None,
            }]
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "ollama".to_owned(),
                api_key: None,
            }
        );
    }

    #[test]
    fn provider_picker_enter_cannot_list_opens_the_free_text_prompt() {
        let mut s = AppState::new();
        // anthropic: native protocol → cannot list, but needs a key.
        s.providers = vec![provider_card(
            "anthropic",
            "Anthropic",
            "anthropic",
            "api-key: ANTHROPIC_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.overlay,
            Overlay::AddModelId {
                provider_id: "anthropic".to_owned(),
                requires_key: true,
                api_key: None,
                buffer: String::new(),
            }
        );
        assert!(s.outbox.is_empty(), "the free-text path emits nothing yet");
    }

    #[test]
    fn provider_picker_tab_and_enter_take_the_same_branch() {
        let providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];

        let mut via_enter = AppState::new();
        via_enter.providers = providers.clone();
        open_provider_picker(&mut via_enter);
        reduce(&mut via_enter, Action::InputSubmit);

        let mut via_tab = AppState::new();
        via_tab.providers = providers;
        open_provider_picker(&mut via_tab);
        reduce(&mut via_tab, Action::BeginAddModel);

        assert_eq!(via_enter.overlay, via_tab.overlay);
        assert!(matches!(via_enter.overlay, Overlay::AddModelProviderKey { .. }));
    }
```

- [ ] **Step 2: Run the new flow tests — verify they fail**

Run: `cargo test -p codypendent-tui provider_picker_enter_can_list provider_picker_enter_cannot_list provider_picker_tab_and_enter 2>&1 | tail -30`
Expected: FAIL — Enter still stages `pending_provider` / Tab still opens `AddModelId`.

- [ ] **Step 3: Add `enter_add_model_flow` and rewrite `begin_add_model`**

In `crates/tui/src/reduce.rs`, replace `begin_add_model` (~1238-1256) with the shared helper + a thin Tab entry:

```rust
/// The shared add-model entry, called by both `Tab` (`begin_add_model`) and
/// `Enter` (the `ProviderPicker` submit arm). Branches on the focused provider's
/// gates (model-discovery):
/// - can-list + hosted → key-first masked prompt (the key is needed before the
///   model name exists), which on submit queries `<base_url>/models`.
/// - can-list + local/no-auth → query immediately (no key).
/// - cannot-list → today's free-text `AddModelId` flow, unchanged.
fn enter_add_model_flow(
    state: &mut AppState,
    provider_id: String,
    requires_key: bool,
    can_list_models: bool,
) {
    state.overlay = if can_list_models && requires_key {
        Overlay::AddModelProviderKey {
            provider_id,
            buffer: SecretKey(String::new()),
        }
    } else if can_list_models {
        state.outbox.push(Intent::QueryProviderModels {
            provider_id: provider_id.clone(),
            api_key: None,
        });
        Overlay::AddModelQuerying {
            provider_id,
            api_key: None,
        }
    } else {
        Overlay::AddModelId {
            provider_id,
            requires_key,
            api_key: None,
            buffer: String::new(),
        }
    };
}

/// Begin the add-model flow (`Tab` in the `/provider` picker) for the focused
/// catalog provider. A no-op outside the provider picker, or when the filtered
/// selection matches no provider (the same zero-match guard the Enter arm uses).
fn begin_add_model(state: &mut AppState) {
    let (provider_id, requires_key, can_list_models) = {
        let Overlay::ProviderPicker { query, selected } = &state.overlay else {
            return;
        };
        let Some(&idx) = filter_providers(&state.providers, query).get(*selected) else {
            return;
        };
        match state.providers.get(idx) {
            Some(card) => (card.id.clone(), card.requires_key, card.can_list_models),
            None => return,
        }
    };
    enter_add_model_flow(state, provider_id, requires_key, can_list_models);
}
```

- [ ] **Step 4: Rewrite the `ProviderPicker` Enter arm to begin the flow (delete the staging + false notice)**

In `crates/tui/src/reduce.rs`, replace the `Overlay::ProviderPicker { query, selected } => { ... }` arm in `submit_prompt` (~1117-1128) with:

```rust
        // Enter begins the add-model flow for the focused provider — the same
        // branch `Tab` takes (model-discovery). The old `pending_provider`
        // staging + "applies to your next run" notice are removed: nothing ever
        // consumed the staged value. Re-derives the filtered selection from the
        // overlay's own `query`/`selected` (the zero-match guard the model picker
        // uses); `mem::take` already closed the picker, so `enter_add_model_flow`
        // sets the next overlay directly.
        Overlay::ProviderPicker { query, selected } => {
            if let Some(&idx) = filter_providers(&state.providers, &query).get(selected) {
                if let Some(card) = state.providers.get(idx) {
                    let provider_id = card.id.clone();
                    let requires_key = card.requires_key;
                    let can_list_models = card.can_list_models;
                    enter_add_model_flow(state, provider_id, requires_key, can_list_models);
                }
            }
        }
```

- [ ] **Step 5: Remove `pending_provider` from `AppState`**

In `crates/tui/src/state.rs`:

Delete the field (~871-875):

```rust
    /// The provider staged from the picker (`Enter` on a row). Advisory/
    /// browse-only this task — nothing yet reads it to change which provider
    /// serves a run; wiring a staged provider into a live run (including the
    /// auth state machine) is a follow-up.
    pub pending_provider: Option<String>,
```

Delete its `new()` initializer (~959):

```rust
            pending_provider: None,
```

Update the `ProviderPicker` overlay doc (~160-163) — the last sentence now reads:

```rust
    /// (reset to 0 whenever the query changes) — the same shape as
    /// [`Overlay::ModelPicker`]. `Enter` (or `Tab`) begins the add-model flow
    /// for the focused provider (model-discovery).
    ProviderPicker { query: String, selected: usize },
```

- [ ] **Step 6: Remove the `state.pending_provider` staged marker in `render_provider_picker`**

In `crates/tui/src/render.rs`, in `render_provider_picker`:

Delete the `staged` binding (~1429-1431):

```rust
    // The provider already staged for the next run, if any — marks the
    // staged row/detail.
    let staged = state.pending_provider.as_deref();
```

In the list-row loop, remove the `is_staged` binding (~1450) and collapse the marker span (~1456-1459) to a constant spacer so the id column stays aligned:

```rust
        let is_selected = row == selected;
        let head = Line::from(vec![
            Span::styled(
                if is_selected { "› " } else { "  " },
                Style::default().fg(theme.focus.active),
            ),
            Span::styled("  ", Style::default().fg(theme.focus.active)),
            Span::styled(
                truncate(&card.id, 26),
                Style::default().fg(theme.text.primary),
            ),
        ]);
```

In the detail panel, remove the `is_staged` binding (~1500) and replace the heading's staged suffix (~1501-1516) with the plain id:

```rust
    if let Some(card) = state.focused_provider() {
        lines.push(Line::from(vec![Span::styled(
            card.id.clone(),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        )]));
```

_(The two hint strings at ~1537/1548 are updated in Task 5; they are inert copy — compiling and correct behavior do not depend on them.)_

- [ ] **Step 7: Update the `BeginAddModel` action doc**

In `crates/tui/src/action.rs`, update the `BeginAddModel` doc (~164-167):

```rust
    /// Begin the add-model flow for the focused provider in the `/provider`
    /// picker (`Tab`; `Enter` does the same). Branches on the provider's gates:
    /// a can-list provider queries its `/models` list; a cannot-list one opens
    /// the free-text name prompt. A no-op outside the provider picker.
    BeginAddModel,
```

- [ ] **Step 8: Rewrite the existing tests that assumed the old Enter/Tab behavior**

In `crates/tui/src/reduce.rs`, update these tests (~3699-3932). Each change is because groq/ollama are now can-list and Enter no longer stages:

Replace `provider_picker_enter_stages_the_focused_provider_and_emits_a_notice` (~3699-3727) with a local-branch test (it focuses ollama):

```rust
    #[test]
    fn provider_picker_enter_begins_the_flow_for_the_focused_provider() {
        let mut s = AppState::new();
        s.providers = vec![
            provider_card("groq", "Groq", "openai-chat", "api-key: GROQ_API_KEY", false),
            provider_card("ollama", "Ollama (local)", "openai-chat", "none", true),
        ];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::SelectNext); // focus "ollama" (can-list local)
        reduce(&mut s, Action::InputSubmit); // Enter begins the flow

        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "ollama".to_owned(),
                api_key: None,
            },
            "the picker gives way to the add-model flow, not a staged marker"
        );
        assert_eq!(
            s.outbox,
            vec![Intent::QueryProviderModels {
                provider_id: "ollama".to_owned(),
                api_key: None,
            }]
        );
    }
```

Update `provider_picker_enter_with_zero_matches_stages_nothing` (~3729-3770) — rename and drop the `pending_provider` assertions:

```rust
    #[test]
    fn provider_picker_enter_with_zero_matches_begins_nothing() {
        let mut s = AppState::new();
        s.providers = vec![
            provider_card("groq", "Groq", "openai-chat", "api-key: GROQ_API_KEY", false),
            provider_card("ollama", "Ollama (local)", "openai-chat", "none", true),
        ];
        open_provider_picker(&mut s);
        for c in "zzz-no-such-provider".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        assert!(
            crate::state::filter_providers(&s.providers, "zzz-no-such-provider").is_empty(),
            "precondition: the query must match nothing"
        );

        reduce(&mut s, Action::InputSubmit);

        assert_eq!(s.overlay, Overlay::None, "the picker still closes");
        assert!(s.outbox.is_empty(), "a zero-match submit must begin no flow");
    }
```

Update `provider_picker_escape_closes_without_staging` (~3772-3786) — drop the `pending_provider` line:

```rust
    #[test]
    fn provider_picker_escape_closes_the_picker() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::InputCancel);
        assert_eq!(s.overlay, Overlay::None);
        assert!(s.outbox.is_empty(), "Esc begins no flow");
    }
```

Replace `provider_picker_tab_begins_the_add_model_flow_for_the_focused_provider` (~3790-3814) — groq is now can-list+hosted, so Tab opens the key prompt:

```rust
    #[test]
    fn provider_picker_tab_begins_the_add_model_flow_for_the_focused_provider() {
        let mut s = AppState::new();
        s.providers = vec![
            provider_card("groq", "Groq", "openai-chat", "api-key: GROQ_API_KEY", false),
            provider_card("ollama", "Ollama (local)", "openai-chat", "none", true),
        ];
        open_provider_picker(&mut s); // focuses row 0 (groq)
        reduce(&mut s, Action::BeginAddModel);
        assert_eq!(
            s.overlay,
            Overlay::AddModelProviderKey {
                provider_id: "groq".to_owned(),
                buffer: SecretKey(String::new()),
            }
        );
        assert_eq!(s.input_mode(), crate::state::InputMode::Editing);
    }
```

Replace `add_model_hosted_flow_prompts_for_a_key_then_emits_the_intent` (~3816-3856) — the name→key path now belongs to a *cannot-list* hosted provider (anthropic-native):

```rust
    #[test]
    fn add_model_cannot_list_hosted_flow_prompts_name_then_key_then_emits() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "anthropic",
            "Anthropic",
            "anthropic",
            "api-key: ANTHROPIC_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel); // cannot-list → free-text name prompt
        assert!(matches!(s.overlay, Overlay::AddModelId { requires_key: true, .. }));
        for c in "claude-haiku-4.5".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit); // name → masked key
        assert_eq!(
            s.overlay,
            Overlay::AddModelKey {
                provider_id: "anthropic".to_owned(),
                model: "claude-haiku-4.5".to_owned(),
                buffer: SecretKey(String::new()),
            }
        );
        assert!(s.outbox.is_empty(), "no intent until the key is entered");

        for c in "sk-secret".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit); // key → emit
        assert_eq!(s.overlay, Overlay::None);
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "anthropic/claude-haiku-4.5".to_owned(),
                provider_id: "anthropic".to_owned(),
                model: "claude-haiku-4.5".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }]
        );
    }
```

Replace `add_model_local_provider_skips_the_key_step_and_emits_no_key` (~3858-3891) — the keyless name-typed path now belongs to a *cannot-list* keyless provider (ACP):

```rust
    #[test]
    fn add_model_cannot_list_keyless_flow_skips_the_key_step() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "claude-code",
            "Claude Code (ACP)",
            "acp",
            "acp: npx",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel); // cannot-list, no key → name prompt
        assert!(matches!(s.overlay, Overlay::AddModelId { requires_key: false, .. }));
        for c in "some-model".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit); // no key step → emit directly
        assert_eq!(s.overlay, Overlay::None);
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "claude-code/some-model".to_owned(),
                provider_id: "claude-code".to_owned(),
                model: "some-model".to_owned(),
                api_key: None,
            }]
        );
    }
```

Update `add_model_rejects_a_blank_model_name` (~3893-3912) and `add_model_escape_abandons_the_flow_without_emitting` (~3914-3932) — swap the groq fixture for the cannot-list `anthropic` fixture so `Tab` still lands on `AddModelId`:

```rust
    #[test]
    fn add_model_rejects_a_blank_model_name() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "anthropic",
            "Anthropic",
            "anthropic",
            "api-key: ANTHROPIC_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel);
        reduce(&mut s, Action::InputSubmit); // empty buffer
        assert!(
            matches!(s.overlay, Overlay::AddModelId { .. }),
            "the prompt stays open on a blank name"
        );
        assert!(s.outbox.is_empty(), "no intent for a blank model name");
        assert!(s.notice.is_some(), "a notice explains the rejection");
    }

    #[test]
    fn add_model_escape_abandons_the_flow_without_emitting() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "anthropic",
            "Anthropic",
            "anthropic",
            "api-key: ANTHROPIC_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel);
        for c in "x".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputCancel); // Esc on the model-name prompt
        assert_eq!(s.overlay, Overlay::None);
        assert!(s.outbox.is_empty());
    }
```

- [ ] **Step 9: Rewrite the render test that set `pending_provider`**

In `crates/tui/src/render.rs`, replace `provider_picker_snapshot_shows_rows_staged_marker_and_badges` (~4334-4406) with a version that drops the staged marker (both `ProviderCard` literals keep the `can_list_models: true` added in Task 1):

```rust
    #[test]
    fn provider_picker_snapshot_shows_rows_and_badges() {
        let mut state = running_build_state();
        state.providers = vec![
            ProviderCard {
                id: "groq".to_owned(),
                name: "Groq".to_owned(),
                protocol: "openai-chat".to_owned(),
                auth: "api-key: GROQ_API_KEY".to_owned(),
                local: false,
                requires_key: true,
                can_list_models: true,
            },
            ProviderCard {
                id: "ollama".to_owned(),
                name: "Ollama (local)".to_owned(),
                protocol: "openai-chat".to_owned(),
                auth: "none".to_owned(),
                local: true,
                requires_key: false,
                can_list_models: true,
            },
        ];
        reduce(&mut state, Action::OpenPalette);
        for c in "provider".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        reduce(&mut state, Action::InputSubmit);
        reduce(&mut state, Action::SelectNext);
        assert!(matches!(state.overlay, Overlay::ProviderPicker { .. }));

        let text = render_to_string(&state, 120, 40);
        assert!(text.contains("Provider catalog"), "title missing:\n{text}");
        assert!(text.contains("groq"), "first row missing:\n{text}");
        assert!(text.contains("ollama"), "second row missing:\n{text}");
        assert!(text.contains("Groq"), "first row's name missing:\n{text}");
        assert!(text.contains("Ollama (local)"), "second row's name missing:\n{text}");
        assert!(text.contains("openai-chat"), "protocol missing:\n{text}");
        assert!(text.contains("api-key: GROQ_API_KEY"), "auth badge missing:\n{text}");
        assert!(text.contains("none"), "auth badge missing:\n{text}");
        assert!(text.contains("hosted"), "hosted badge missing:\n{text}");
        assert!(text.contains("local \u{2713}"), "local badge missing:\n{text}");
        // Staging is gone: no staged marker should render.
        assert!(!text.contains("● staged"), "the dead staged marker must not render:\n{text}");
    }
```

- [ ] **Step 10: Run the whole tui + cli suites — verify pass**

Run: `cargo test -p codypendent-tui && cargo test -p codypendent-cli 2>&1 | tail -20`
Expected: PASS (flow tests green; rewritten tests green; `pending_provider` fully gone — no compile references remain).

- [ ] **Step 11: Clippy + commit**

Run: `cargo clippy -p codypendent-tui -p codypendent-cli --all-targets -- -D warnings`
Expected: no warnings (verify no dead-code warning for a now-unused helper).

```bash
git add crates/tui/src/reduce.rs crates/tui/src/state.rs crates/tui/src/render.rs crates/tui/src/action.rs
git commit -m "feat(tui): Enter/Tab begin the add-model flow; remove dead pending_provider"
```

---

## Task 5: Provider-picker discoverability hint

The provider picker's copy still says "Enter stages this provider for your next run" — stale after Task 4. Replace the body hint and the footer so they truthfully describe the add-model flow, and assert the new copy.

**Files:**
- Modify: `crates/tui/src/render.rs` (`render_provider_picker` hint ~1537, footer ~1548; add a render assertion)

**Interfaces:**
- Consumes: nothing new. Pure copy + a render test.

- [ ] **Step 1: Write the failing hint test**

In `crates/tui/src/render.rs`, in the render `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn provider_picker_hint_says_add_model_not_stage() {
        let mut state = running_build_state();
        state.providers = vec![ProviderCard {
            id: "groq".to_owned(),
            name: "Groq".to_owned(),
            protocol: "openai-chat".to_owned(),
            auth: "api-key: GROQ_API_KEY".to_owned(),
            local: false,
            requires_key: true,
            can_list_models: true,
        }];
        reduce(&mut state, Action::OpenPalette);
        for c in "provider".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        reduce(&mut state, Action::InputSubmit);
        assert!(matches!(state.overlay, Overlay::ProviderPicker { .. }));

        let text = render_to_string(&state, 120, 40);
        assert!(
            text.contains("add model") || text.contains("browse this provider's models"),
            "the hint must describe adding a model, not staging:\n{text}"
        );
        assert!(!text.contains("stage"), "the dead 'stage' copy must be gone:\n{text}");
    }
```

- [ ] **Step 2: Run — verify it fails**

Run: `cargo test -p codypendent-tui provider_picker_hint_says_add_model_not_stage 2>&1 | tail -20`
Expected: FAIL — the current copy still says "stage".

- [ ] **Step 3: Update the two hint strings**

In `crates/tui/src/render.rs`, in `render_provider_picker`:

Body hint (~1536-1539):

```rust
        lines.push(Line::styled(
            "  Enter or Tab — browse this provider's models to add one",
            Style::default().fg(theme.text.muted),
        ));
```

Footer (~1546-1550):

```rust
    lines.push(Line::styled(
        "  ↑/↓ select · Enter/Tab add model · Esc close",
        Style::default().fg(theme.text.muted),
    ));
```

- [ ] **Step 4: Run — verify pass**

Run: `cargo test -p codypendent-tui provider_picker_hint_says_add_model_not_stage provider_picker_snapshot_shows_rows_and_badges 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Full suite + clippy + commit**

Run: `cargo test -p codypendent-tui && cargo test -p codypendent-cli && cargo clippy -p codypendent-tui -p codypendent-cli --all-targets -- -D warnings`
Expected: all PASS, no warnings.

```bash
git add crates/tui/src/render.rs
git commit -m "feat(tui): truthful /provider picker hint for the add-model flow"
```

---

## Self-Review

**1. Spec coverage** (each spec section → task):

- Problem gaps (no discovery; dead `Enter`) → Tasks 4 (Enter/Tab begin flow) + 2/3 (discovery machinery).
- Target flow #1 (Enter or Tab begins) → Task 4 (`enter_add_model_flow`, both entries).
- Target flow #2 (can-list hosted key-first → query → pick; local → query → pick) → Task 4 branch + Task 2 (`AddModelProviderKey`/`AddModelPick` submit) + Task 3 (GET).
- Target flow #3 (cannot-list → free-text unchanged) → Task 4 (`AddModelId` branch) + Task 1 (`can_list_models` gate false).
- Target flow #4 (fallback carrying the key) → Task 2 (`on_provider_models_failed` + `AddModelId` `api_key` branch).
- Endpoint correction `<base_url>/models` → Task 3 (`models_url` + its test asserting no doubled `/v1`, incl. z.ai `/v4`).
- HTTP stack (reqwest workspace, auth header convention, 10s timeout) → Task 3 (`query_provider_models`, `crates/cli/Cargo.toml`).
- Client-only shapes: `Intent::QueryProviderModels`, `Action::ProviderModelsLoaded/Failed`, 3 overlays, `AddModelId.api_key`, `ProviderCard.can_list_models`, remove `pending_provider`, `filter_model_names` → Tasks 1 + 2 (+ removal in Task 4).
- Architecture §1 (reducer: `enter_add_model_flow`, Tab/Enter rewrite, submit arms, Action arms, `edit_prompt`, `nav`) → Tasks 2 + 4.
- Architecture §2 (input via `input_mode()`, no key-map fns) → Task 2 (`input_mode()`); confirmed no `input.rs` change (the palette `Tab → BeginAddModel` is a harmless no-op in `AddModelPick` because `begin_add_model` returns early outside the provider picker).
- Architecture §3 (render the 3 overlays; hint fix; remove marker) → Task 2 (overlay render arms — pulled here by the exhaustive `render_overlays` match) + Task 4 (marker removal) + Task 5 (hint copy).
- Architecture §4 (harness: `ReaderSignal::ProviderModels`, query task, `query_provider_models`, response parsing, `provider_can_list_models`, Cargo.toml) → Tasks 1 + 3.
- Data flow, error/edge cases (race guard, blank key, empty list, dismiss) → Tasks 2 (`on_provider_models_loaded/failed` race guard + tests) + 3 (empty→Err, non-2xx→status-only).
- Testing section → each task's tests mirror the spec's bullet list (reducer transitions, redaction, parsing, URL, render masking).
- Constraints → the Global Constraints block, verbatim.
- Open questions #1 (keep `AddModelKey`) → kept; the cannot-list-but-needs-key path uses it (Task 4's anthropic test). #2 (`AddModelQuerying` as overlay) → overlay, per spec.

**2. Placeholder scan:** No `TODO`/`TBD`/"handle errors"/"similar to Task N". Every code step shows complete code; every reason string is a literal; the one `=> {}`-style construct (`render_querying` is a full function, not a stub) is real.

**3. Type/signature consistency across tasks:**
- `Intent::QueryProviderModels { provider_id: String, api_key: Option<SecretKey> }` — identical in Task 2 (def), Task 2/4 (emit), Task 3 (`intent_to_command` arm + interception).
- `Action::ProviderModelsLoaded { provider_id, models: Vec<String> }` / `ProviderModelsFailed { provider_id, reason: String }` — identical in Task 2 (def + handlers) and Task 3 (`select!` mapping).
- `ReaderSignal::ProviderModels { provider_id: String, result: Result<Vec<String>, String> }` — Task 3 only.
- `query_provider_models(&str, &str, &str, Option<&str>) -> Result<Vec<String>, String>` — def + call site both Task 3.
- `filter_model_names(&[String], &str) -> Vec<usize>` — def (Task 2 state.rs), used in reduce (Task 2) and render (Task 2). Imported in both `reduce.rs` and `render.rs`.
- `ProviderCard.can_list_models` — added Task 1, read in Task 4 (`enter_add_model_flow` inputs via `begin_add_model` / Enter arm).
- `Overlay::AddModelId` field order `{ provider_id, requires_key, api_key, buffer }` — consistent across state def, reduce arms, and every test literal.
- `enter_add_model_flow(state, provider_id, requires_key, can_list_models)` — one signature, three call paths (Tab, Enter). Consistent.

No gaps or mismatches found. Two deliberate cross-task notes recorded inline: (a) Task 3 precedes Task 4 so the harness interception exists before the flow emits `QueryProviderModels`; (b) the provider-picker hint copy lags one task (fixed in Task 5) — a compile-safe, behavior-safe copy lag, since the `render_overlays` exhaustive match forced the new-overlay rendering into Task 2 and the `pending_provider`-reference removal into Task 4.

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-07-26-model-discovery.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
