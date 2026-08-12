# Hybrid Onboard + Discovery Trust Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When zero runnable models exist, open a trustworthy hybrid onboarding flow; then deliver the competitive agent-workbench and curated-learning requirements in design §§16–17.

**Architecture:** Keep `crates/tui` pure. Prefer **Onboard architecture A**: Triage + SkipConfirm only; set `AppState.onboard_active` and hand off to existing `enter_add_model_flow` / ACP paths. Harness persists prefs on `SessionStore`, resolves env keys for `has_key`/queries, stamps `request_id` on model-list round-trips, and pins `pending_model` + `SetOnboardComplete` on success.

**Tech Stack:** Rust workspace — `codypendent-tui` (reducer/render/input), `codypendent-cli` (harness I/O), `codypendent-providers` / `codypendent-runtime` (catalog + `auth.json`). Ratatui + crossterm. Existing `reqwest` for `/models`.

**Spec:** `docs/superpowers/specs/2026-08-12-hybrid-onboard-discovery-design.md`

## Global Constraints

- **No I/O in `crates/tui`.** Env/`auth.json`/HTTP only in `crates/cli/src/tui.rs` (and existing runtime helpers).
- **No OAuth UI.** ACP for agents.
- **Client-only intents** for onboard prefs + AddModel/Query — never `CommandBody` / daemon wire.
- **Do not implement** OAuth, arbitrary shell-executed status scripts, silent learning from untrusted content, or raw-transcript memory capture.
- **Prefer architecture A** (Onboard triage → existing add-model overlays).
- **Secret hygiene:** keys only in `SecretKey` / harness; never in Actions, logs, or notices.
- **Lint:** `cargo clippy --all-targets` clean on Linux CI (`-D warnings`).
- **Tests first** per task; rewrite tests that currently encode bad blank-key behavior.

## File map

| File | Responsibility |
|------|----------------|
| `crates/tui/src/state.rs` | `Overlay::Onboard`, `OnboardStep`, `AppState.onboard_active`, optional `provider_models_generation` |
| `crates/tui/src/action.rs` | `request_id` on Query/Loaded/Failed; `Intent::SetOnboardComplete` / `SetOnboardSkipped` |
| `crates/tui/src/reduce.rs` | Onboard nav/submit; blank-key reject; request_id ignore; set `onboard_active`; complete on add success path signals |
| `crates/tui/src/render.rs` | Onboard UI; splash honesty if stage passed in; empty CTA hint |
| `crates/tui/src/input.rs` / `accessible.rs` | Route Onboard like other overlays; announce triage |
| `crates/tui/src/palette.rs` | Optional command to reopen onboard (only if needed for CTA) |
| `crates/cli/src/tui.rs` | SessionStore fields; splash stage; post-splash gate; env `has_key`; query key+headers; request_id; pin + prefs drain; reload providers after add |

---

### Task 1: Env-aware `has_key` + query credentials

**Files:**
- Modify: `crates/cli/src/tui.rs` (`load_provider_cards` `has_key` ~5784; `QueryProviderModels` drain ~1980–2116; `query_provider_models` ~4714; `stored_provider_key` helpers; tests)
- Test: `crates/cli/src/tui.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `fn provider_has_resolvable_key(provider_id, auth: &AuthStore, catalog_envs: &[String]) -> bool`
- Produces: query uses overlay key ∨ auth.json ∨ **first set env** from catalog AuthMethod
- Produces: `query_provider_models` applies `extra_headers` from catalog provider when present
- Consumes: `provider_auth_id`, catalog `AuthMethod::ApiKey { env, .. }`, `std::env::var`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn provider_has_resolvable_key_true_when_env_set_and_auth_empty() {
    // set env via temp var unique to test; empty AuthStore;
    // assert provider_has_resolvable_key(...)
}

#[test]
fn provider_has_resolvable_key_true_when_auth_json_has_provider_key() {
    // auth inserted at provider_auth_id; env unset; assert true
}

#[test]
fn provider_has_resolvable_key_false_when_neither() {
    // assert false
}
```

Also add a unit test that `query_provider_models` (or a small helper extracting header construction) includes Authorization when only env supplies the key — if HTTP is hard to unit-test, test a pure `resolve_provider_api_key(...) -> Option<String>` helper instead and call it from both `has_key` and the query drain.

- [ ] **Step 2: Run tests — expect FAIL**

```bash
cargo test -p codypendent-cli provider_has_resolvable_key -- --nocapture
```

- [ ] **Step 3: Implement helpers + wire `load_provider_cards` + query drain**

```rust
fn resolve_provider_api_key(
    provider_id: &str,
    auth: &AuthStore,
    env_names: &[String],
) -> Option<String> {
    if let Some(key) = auth
        .get(&provider_auth_id(provider_id))
        .filter(|k| !k.is_empty())
    {
        return Some(key.to_string());
    }
    for name in env_names {
        if let Ok(v) = std::env::var(name) {
            let t = v.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}
```

Use catalog provider’s ApiKey env list when building cards and when draining `QueryProviderModels` (if intent `api_key` is None).

Pass `extra_headers` into the GET builder from the catalog provider entry.

After `apply_add_model`, reload **providers** as well as models/key statuses:

```rust
state.providers = load_provider_cards(...).await;
```

- [ ] **Step 4: Run tests — expect PASS**

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/tui.rs
git commit -m "$(cat <<'EOF'
fix(tui): resolve provider keys from auth.json or env for discovery

EOF
)"
```

---

### Task 2: `request_id` on provider model list round-trips

**Files:**
- Modify: `crates/tui/src/action.rs` (`Intent::QueryProviderModels`, `Action::ProviderModelsLoaded`, `ProviderModelsFailed`)
- Modify: `crates/tui/src/state.rs` (`AppState` counter, e.g. `provider_models_req: u64`)
- Modify: `crates/tui/src/reduce.rs` (all emit sites + `on_provider_models_loaded` / `_failed`)
- Modify: `crates/cli/src/tui.rs` (drain + signal mapping must forward `request_id`)
- Modify: every test constructing these variants

**Interfaces:**
- Produces: `request_id: u64` on Intent + both Actions
- Produces: ignore Loaded/Failed when `request_id` ≠ overlay’s expected id (store expected id on `AddModelQuerying` / `AddModelPick` **or** compare to `AppState.provider_models_latest`)

Recommended overlay fields:

```rust
AddModelQuerying { provider_id, api_key, request_id: u64 }
AddModelPick { ..., request_id: u64, ... }
```

When emitting Intent, bump `state.provider_models_req` and stamp that id on Intent + overlay.

- [ ] **Step 1: Write failing reducer tests**

```rust
#[test]
fn provider_models_loaded_ignores_stale_request_id() {
    // Open AddModelPick with request_id=2, Live rows.
    // Dispatch ProviderModelsLoaded { request_id: 1, origin: Catalog, ... }
    // Assert models/origin unchanged; still Live.
}

#[test]
fn provider_models_loaded_applies_matching_request_id() {
    // request_id matches → rows update
}
```

- [ ] **Step 2: Run — expect FAIL / compile errors until types updated**

- [ ] **Step 3: Thread `request_id` through action/state/reduce/cli; update all call sites**

Soft-fail path that remaps Failed→Catalog Loaded must keep the **same** `request_id`.

- [ ] **Step 4: `cargo test -p codypendent-tui provider_models_` and cli compile**

- [ ] **Step 5: Commit**

```bash
git commit -m "$(cat <<'EOF'
fix(tui): ignore stale ProviderModelsLoaded with request generations

EOF
)"
```

---

### Task 3: Reject blank `AddModelKey` / tighten `AddModelProviderKey`

**Files:**
- Modify: `crates/tui/src/reduce.rs` submit arms ~4287–4330
- Modify: tests that assert blank→query/`AddModel` (rewrite)

**Interfaces:**
- `AddModelKey` blank → no Intent; notice optional `"API key required"`; overlay stays
- `AddModelProviderKey` blank → if provider `requires_key` and card `!has_key` → reject; if `has_key` (env/auth) → query with `api_key: None` (harness resolves env)

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn add_model_key_blank_emits_nothing() {
    // overlay AddModelKey buffer ""; submit; assert outbox empty
}

#[test]
fn add_model_provider_key_blank_rejects_when_no_has_key() {
    // card requires_key, has_key false; blank submit; no QueryProviderModels
}
```

Delete or invert `add_model_provider_key_blank_queries_with_no_key`.

- [ ] **Step 2–4: Implement + pass + commit**

```bash
git commit -m "$(cat <<'EOF'
fix(tui): reject blank API keys in add-model hosted prompts

EOF
)"
```

---

### Task 4: SessionStore onboard prefs + intents

**Files:**
- Modify: `crates/cli/src/tui.rs` `SessionStore` ~6836; theme drain pattern ~2256; boot load
- Modify: `crates/tui/src/action.rs` intents
- Modify: `crates/tui/src/reduce.rs` (emit on skip/complete — complete may wait until Task 6)
- Test: round-trip save/load JSON

**Interfaces:**

```rust
// SessionStore
#[serde(default, skip_serializing_if = "Option::is_none")]
onboarded: Option<bool>,
#[serde(default, skip_serializing_if = "Option::is_none")]
onboard_skipped: Option<bool>,

// Intent
SetOnboardComplete,
SetOnboardSkipped,
```

Harness:

```rust
Intent::SetOnboardComplete => {
    store.onboarded = Some(true);
    store.onboard_skipped = None;
    store.save(paths);
}
Intent::SetOnboardSkipped => {
    store.onboard_skipped = Some(true);
    store.save(paths);
}
```

Load at boot into locals used by post-splash gate (Task 5). Optionally mirror onto `AppState` if CTA needs it.

- [ ] **Step 1: Test SessionStore serde round-trip with new fields**
- [ ] **Step 2–4: Implement + drain + commit**

```bash
git commit -m "$(cat <<'EOF'
feat(tui): persist onboarded and onboard_skipped in SessionStore

EOF
)"
```

---

### Task 5: Runnable helper + splash honesty + post-splash gate

**Files:**
- Modify: `crates/cli/src/tui.rs` ready_stage ~360; after `wait_for_splash_entry`; helpers
- Modify: splash tests in `crates/tui/src/render.rs` and/or cli gate tests
- Possibly: seed a `key_resolvable` bit when building `ModelCard` **or** compute runnable in harness from models + auth + env

**Interfaces:**

```rust
fn splash_ready_stage(workspace_name: &str, runnable_count: usize) -> String {
    if runnable_count == 0 {
        format!("set up a model to continue") // or include workspace
    } else {
        format!("{workspace_name} is ready")
    }
}

fn should_open_onboard(runnable_count: usize, onboard_skipped: bool) -> bool {
    runnable_count == 0 && !onboard_skipped
}
```

After Enter, if `should_open_onboard`:

```rust
state.overlay = Overlay::Onboard {
    step: OnboardStep::Triage { selected: 0 },
};
```

(Requires Task 6 types — if sequencing requires compile, stub `Overlay::Onboard` in Task 5 minimally or merge 5+6.)

**Runnable count:** Prefer harness function using loaded `ModelCard`s + `resolve_provider_api_key` / ACP readiness. Document: hosted `Unverified` + resolvable key ⇒ counts as runnable.

- [ ] **Step 1: Unit tests for `splash_ready_stage` and `should_open_onboard`**
- [ ] **Step 2: Update splash render test that asserts `"is ready"` — split ready vs empty cases**
- [ ] **Step 3: Wire boot path**
- [ ] **Step 4: Pass + commit**

```bash
git commit -m "$(cat <<'EOF'
fix(tui): honest splash copy and open onboard when no runnable models

EOF
)"
```

---

### Task 6: `Overlay::Onboard` Triage + SkipConfirm (architecture A)

**Files:**
- Modify: `crates/tui/src/state.rs` — enum + `AppState.onboard_active: bool`
- Modify: `reduce.rs`, `render.rs`, `input.rs` / `input_mode()`, `accessible.rs`
- Test: reducer navigation + render smoke + accessible string

**Interfaces:**

```rust
pub enum OnboardStep {
    Triage { selected: usize },
    SkipConfirm { selected: usize }, // 0 Skip forever, 1 Continue setup, 2 Cancel
}

// Overlay::Onboard { step: OnboardStep }

// AppState
pub onboard_active: bool, // default false
```

Triage rows (fixed 3): Hosted / Local / ACP.

- Esc on Triage → `SkipConfirm { selected: 0 }`
- SkipConfirm: Enter on Skip forever → `Intent::SetOnboardSkipped`, `onboard_active=false`, `overlay=None`
- Continue → back to Triage
- Cancel → Triage
- Enter on Hosted → `onboard_active=true`; open provider filter or `ProviderPicker` pre-filtered — simplest MVP: `Overlay::ProviderPicker { query: "", selected: first_hosted }` **or** call helper that lists only hosted cards. If ProviderPicker can’t filter by kind, open picker and rely on operator — better: new thin step OR select first matching provider and `enter_add_model_flow`.

**MVP selection behavior (be precise):**

1. **Hosted:** find first `providers` where `!local && requires_key && available` (or open ProviderPicker with query empty and `selected` on first such). Preferred: set overlay to `ProviderPicker` with selection on first hosted available — user can move. Set `onboard_active=true`.
2. **Local:** same for `local && available`.
3. **ACP:** first ACP provider card (`is_acp_provider` / protocol) or ProviderPicker focused there; `enter_add_model_flow` semantics on Enter already handle ACP.

Alternatively Triage Enter **immediately** calls `enter_add_model_flow(state, provider_id)` for the first matching provider of that class — faster but less choice. **Prefer open ProviderPicker focused on that class** so user can choose.

- [ ] **Step 1: Reducer tests for Esc/Skip/Continue and setting `onboard_active`**
- [ ] **Step 2: Render triage + skip confirm (theme tokens only)**
- [ ] **Step 3: Accessible announcement includes “Hosted”, “Local”, “ACP”**
- [ ] **Step 4: Commit**

```bash
git commit -m "$(cat <<'EOF'
feat(tui): add Onboard triage and skip-confirm overlay

EOF
)"
```

---

### Task 7: Complete onboard on successful add + pin `pending_model`

**Files:**
- Modify: `crates/cli/src/tui.rs` `apply_add_model` ~4042; `AcpConnected` ~1815
- Modify: `crates/tui` if completion is Action-driven (`Action::ModelAdded { display_id }` — only if needed)

**Behavior:**

When add succeeds:

```rust
state.pending_model = Some(display_id.clone());
if state.onboard_active {
    state.onboard_active = false;
    state.overlay = Overlay::None;
    // emit or directly: store onboard complete
    store.onboarded = Some(true);
    store.onboard_skipped = None;
    store.save(paths);
    state.notice = Some((format!("connected {display_id}"), ...));
}
```

If TUI cannot see store, prefer: harness sets prefs; sends `Action` that clears `onboard_active` + overlay; **or** TUI emits `SetOnboardComplete` from a new `Action::AddModelSucceeded` folded in reduce.

**Simplest:** harness-only complete (harness already mutates state after write) — set prefs + `pending_model` + clear overlay/flag in `apply_add_model` / ACP handler when `state.onboard_active`.

- [ ] **Step 1: Test apply_add_model pins pending_model when onboard_active** (cli test with temp dir)
- [ ] **Step 2: Implement pin always on successful add from onboard; consider always pinning on any interactive add (product: onboard must; optional general improvement — **do pin whenever onboard_active**; optionally pin on all AddModel success for consistency with ACP)**
- [ ] **Step 3: Commit**

```bash
git commit -m "$(cat <<'EOF'
feat(tui): pin model and mark onboard complete after successful add

EOF
)"
```

---

### Task 8: Empty Chat CTA reopens Onboard

**Files:**
- Modify: `crates/tui/src/render.rs` empty hero copy ~1622
- Modify: `reduce.rs` / `input.rs` — map a key (Enter when empty+no overlay, or palette) to open Onboard Triage **without** clearing skip

**Behavior:** When `runs.is_empty() && models.is_empty() && overlay==None`, Enter opens `Overlay::Onboard { Triage }` (even if skipped). Do not emit `SetOnboardSkipped` clear.

Update copy: mention Enter to set up / Esc not required.

- [ ] **Step 1: Reducer/input test**
- [ ] **Step 2: Render string assertion**
- [ ] **Step 3: Commit**

```bash
git commit -m "$(cat <<'EOF'
feat(tui): empty chat CTA reopens onboarding wizard

EOF
)"
```

---

### Task 9: Workspace verification

- [ ] **Step 1: Run focused tests**

```bash
cargo test -p codypendent-tui --lib
cargo test -p codypendent-cli --lib provider_has_resolvable_key
cargo test -p codypendent-cli --lib splash_
cargo test -p codypendent-cli --lib onboard
```

- [ ] **Step 2: Clippy**

```bash
cargo clippy -p codypendent-tui -p codypendent-cli --all-targets -- -D warnings
```

- [ ] **Step 3: Manual smoke (if terminal available)**

1. Empty models.toml → splash not “is ready” → Enter → Onboard
2. Env `OPENAI_API_KEY` (or groq) → Hosted path skips/pre-fills → pick → Chat with model
3. Skip forever → relaunch skips wizard → Enter on empty CTA reopens
4. Blank key rejected

- [ ] **Step 4: Final commit if any fixups**

---

## Spec coverage checklist

| Spec section | Task |
|--------------|------|
| §6.1 env has_key + headers + reload providers | 1 |
| §6.2 request_id | 2 |
| §6.3 blank key | 3 |
| §4.4 prefs | 4 |
| §4.1–4.3 splash + gate + runnable | 5 |
| §5.1 / §4.5 Triage + Skip | 6 |
| §5.2–5.4 complete + pin | 7 |
| §5.5 CTA | 8 |
| §8–9 acceptance | 9 |

## Out of scope reminders

OAuth, arbitrary shell status scripts, approvals Esc, OLLAMA_HOST (optional drive-by only if Local unblock needed), and silent/raw memory capture.

---

## Phase 2 — Competitive workbench and curated growth

These tasks are required by design §§16–17. They follow the same tests-first,
pure-reducer and secret-hygiene constraints as Tasks 1–9.

### Task 10: Persistent session strip and larger composer

- [ ] Project model/provider, mode, context, cost, permissions, git/worktree,
      integration health and active/queued subagent counts.
- [ ] Render priority-packed narrow/wide variants without transient notices
      hiding durable state.
- [ ] Make the composer three rows at normal heights, display-width aware and
      explicit about send/queue/steer/interrupt behavior.
- [ ] Add 40/60/80/120-column golden and wrapped-cursor tests.

### Task 11: Provider/model drill-down and command palette

- [ ] Add typed Hosted/LocalEndpoint/AcpAgent filters and provider→model drill-down.
- [ ] Treat bare ACP rows as supplier entries and query their model catalogue.
- [ ] Support filter, wheel/arrows, page/home/end, readiness/auth/context/cost,
      Retry/Test, pin and delete actions.
- [ ] Make palette rows responsive and collision-free for long content.
- [ ] Test configured-but-unrunnable and supplier-with-many-models cases.

### Task 12: Council and subagent workbench

- [ ] Render live worker role, provider/model, task, status, elapsed time and result.
- [ ] Support keyboard/mouse focus, inspect, steer, stop and retry.
- [ ] Validate the council builder end-to-end and persist reusable council templates.
- [ ] Keep worker state visible in Chat and cooked mode.
- [ ] Persist every terminal council outcome as a linked artifact and render the
      complete chair synthesis in an expandable/copyable terminal card.
- [ ] Add a dedicated council-result retrieval projection/tool; never route a
      council lookup through workflow/blackboard by assumption.

### Task 13: Transcript cards and actionable recovery

- [ ] Use compact expandable cards for tools, diffs, reasoning, learning and council activity.
- [ ] Replace generic ACP/model failures with phase, sanitized cause and recovery actions.
- [ ] Add session undo/checkpoint/resume confidence surfaces where existing domain support exists.
- [ ] Preserve terminal-native drag selection and clipboard copy; add explicit
      copy-latest/copy-card actions and bounded multiline paste tests.

### Task 13a: Workflow, Kanban and Blackboard activation

- [ ] Add clear palette/help/empty-state descriptions and primary actions for
      Kanban tasks, executable Workflow graphs and Blackboard evidence.
- [ ] Add guided creation/open/run/post flows plus examples; expose equivalent
      agent tools without silently conflating councils with workflows.
- [ ] Test a council result explicitly handed off into a Kanban task and a
      blackboard artifact, with user-visible provenance.
- [ ] Register typed agent tools for natural-language create/update/run of
      workflows, Kanban cards, blackboard entries and council definitions/runs.
- [ ] Add prompt→tool integration tests that preview, confirm where required,
      persist and link the result into Chat instead of returning navigation advice.

### Task 14: Curated learning domain and persistence

- [ ] Add typed learning scope, provenance, confidence, state, timestamps,
      expiry, pin and supersession metadata with a backwards-compatible migration.
- [ ] Separate bounded factual memory from on-demand procedural skills and episodes.
- [ ] Enforce capture rejection for greetings, generic completions, raw logs,
      temporary paths, secrets and untrusted tool/web instructions.
- [ ] Add query-boundary isolation, dedupe/conflict and poisoning tests.

### Task 15: Learning capture, journey and skill promotion

- [ ] Produce bounded post-run learning proposals from direct preferences,
      corrections and verified outcomes only.
- [ ] Add `/journey`, editable memory lifecycle actions, Undo and `why used` provenance.
- [ ] Propose reusable skills with prerequisites, steps, verification and pitfalls;
      never activate an untrusted proposal automatically.
- [ ] Allow successful councils to become reusable templates.

### Task 16: Accessibility, responsive and interaction conformance

- [ ] Give every graphical onboarding/workbench/learning action a cooked semantic equivalent.
- [ ] Add modal focus shields, exact visible-row mouse targets and keyboard-only traversal.
- [ ] Test monochrome/high-contrast and 40/60/80/120+ column layouts.

### Task 17: End-to-end acceptance

- [ ] Empty install → Enter → onboarding → hosted env/local/ACP model → runnable Chat.
- [ ] Failure/cancel/retry never strands the user or marks onboarding complete early.
- [ ] Provider/model switch, delete and ACP supplier catalogue work live.
- [ ] Council worker activity remains visible and a completed council saves as a template.
- [ ] A verified lesson is proposed, reviewed, recalled with provenance and deleted.
- [ ] Full Rust/SDK/VSCode checks, clippy, format and migration upgrade tests pass.

### Task 18: Patch release

- [ ] Bump the workspace patch version and lockfile.
- [ ] Update user and architecture documentation plus release notes.
- [ ] Commit only intended files, push the release branch, merge to `main`, tag,
      publish GitHub release assets, and verify installation/update metadata.
