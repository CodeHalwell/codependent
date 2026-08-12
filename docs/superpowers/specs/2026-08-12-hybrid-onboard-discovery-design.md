# Hybrid onboarding (C) + discovery trust — design

**Date:** 2026-08-12
**Status:** implementation contract; expanded after review to include competitive workbench UX and curated learning
**Audience:** software engineer agent
**Companion plan:** [`docs/superpowers/plans/2026-08-12-hybrid-onboard-discovery.md`](../plans/2026-08-12-hybrid-onboard-discovery.md) *(implementation tasks — write after this spec is confirmed)*
**Related specs:**
- [`2026-07-24-tui-add-model-design.md`](./2026-07-24-tui-add-model-design.md)
- [`2026-07-26-model-discovery-design.md`](./2026-07-26-model-discovery-design.md)
- [`2026-07-30-tui-experience-design.md`](./2026-07-30-tui-experience-design.md)
- [`docs/architecture/model-provider-selection.md`](../../architecture/model-provider-selection.md) — **out of scope** (Favorites/Recent/fuzzy)
**Audit evidence:** `.cursor-tmp/tui-audit-r3/{06-env-key,07-provider-race,05-add-model}-*.jsonl`

---

## 0. Package for the engineer

This ship is **not** “one vague markdown.” Deliverables:

| Artifact | Role |
|----------|------|
| **This design** | Product decisions, contracts, current→target behavior, non-goals |
| **Implementation plan** | Bite-sized TDD tasks with exact files, types, and test names |
| **Code inventory (§14)** | Where today’s code lives (line anchors may drift; search symbols) |

**Invariant (never violate):** `crates/tui` is a pure reducer — **no I/O**. File/HTTP/env reads happen only in `crates/cli/src/tui.rs` (and runtime/providers). TUI emits `Intent::*`; harness drains → `Action::*`.

---

## 1. Goal

First-run / empty-model users must never land in a dead Chat that claims the workspace is ready. When there are **zero runnable models**, open **`Overlay::Onboard`**. When models are runnable, go to Chat (Connect CTA only if empty/skipped). In the **same ship**, fix discovery trust so the wizard does not lie or race.

---

## 2. Locked decisions

| Topic | Choice |
|--------|--------|
| Product | **C — hybrid** (wizard only when needed) |
| Ship | **B — onboard + discovery trust** |
| Trigger | Zero **runnable** models; **re-offer** until complete or **skip forever** |
| UI | New **`Overlay::Onboard { step }`** |
| Hosted auth | **API key + env prefill only** — **no OAuth** |
| Agents | **ACP** via existing connect path |
| Approach | Onboard shell + **reuse** AddModel / QueryProviderModels / ACP intents |

---

## 3. Non-goals

- OAuth / device-code / SignInState
- A separate architecture-only model picker disconnected from provider/model discovery
- Sticky approvals Esc trap (separate ship)
- Arbitrary shell-executed status-line plugins (the built-in persistent session strip is in scope)
- Required Azure `base_url` wizard / Anthropic protocol wire-up
- Changing splash expand thresholds (70×12) except **readiness copy**
- Second prefs file (must extend `SessionStore`)

---

## 4. Gate & shell

### 4.1 Post-splash sequence (target)

Today (`crates/cli/src/tui.rs`):

1. Boot splash → models/providers seeded into `AppState`
2. `ready_stage = format!("{workspace_name} is ready")` **always** (~L360)
3. `wait_for_splash_entry` → Enter → `event_loop` with `overlay = None`

Target:

1. Same boot seed
2. Compute `runnable = runnable_model_ids(&state)` (or harness helper using models + key resolution)
3. Splash stage: if `runnable.is_empty()` → **"set up a model to continue"** (or similar); else `"{workspace} is ready"`
4. After Enter: if `runnable.is_empty() && !store.onboard_skipped` → `state.overlay = Overlay::Onboard { step: Triage }` before/at start of `event_loop`
5. Else Chat; if models empty but skipped → empty hero Connect CTA (existing + stronger link to Onboard)

### 4.2 Runnable definition

A configured `ModelCard` is **runnable** when a `StartRun` with that model can succeed without another auth step:

| Kind | Runnable when |
|------|----------------|
| ACP | Present in `models.toml` and launchable (`ModelReadiness::Ready`, or equivalent of today’s ACP ready path) |
| Local (no key) | Present; not `Unavailable`; empty/`none` auth OK |
| Hosted (API key) | Present **and** key resolvable via **auth.json model entry ∪ `provider/{id}` ∪ non-empty env for catalog/`api_key_env`** |
| Catalog phantoms / `available=false` / ProtocolNotWired | **Not** runnable |

Empty `models.toml` ⇒ zero runnable.

**Note:** Boot maps hosted cards to `ModelReadiness::Unverified` today (no cloud call). Unverified **with** a resolvable key is still **runnable** for the gate (operator can start a run; verification is separate `/keys` Ctrl-T). Unverified/Unavailable **without** key ⇒ not runnable.

Suggested pure helper (cli or tui):

```rust
fn model_is_runnable(card: &ModelCard, key_resolvable: bool) -> bool {
    match &card.readiness {
        ModelReadiness::Unavailable(_) => false,
        ModelReadiness::Ready | ModelReadiness::Unverified => {
            // Hosted needing key: key_resolvable must be true.
            // Local/ACP: key_resolvable may be true vacuously.
            !requires_api_key(card) || key_resolvable
        }
    }
}
```

`requires_api_key` / `key_resolvable` are harness-computed when seeding cards **or** derived from `ProviderCard` + auth/env. Prefer attaching a bool on seed (e.g. extend projection) over re-reading env inside `crates/tui`.

### 4.3 Persistence (`SessionStore`)

File: `<data_dir>/tui-sessions.json` — struct today:

```rust
struct SessionStore {
    sessions: HashMap<String, StoredSession>,
    resume_token: Option<String>,
    theme: Option<String>,
    // ADD:
    onboarded: Option<bool>,       // or bool with default false
    onboard_skipped: Option<bool>,
}
```

Use same `#[serde(default, skip_serializing_if = ...)]` style as `theme`.

| Event | Writes |
|-------|--------|
| Onboard success (pinned runnable model) | `onboarded = true`, clear `onboard_skipped` |
| Skip forever | `onboard_skipped = true` |
| CTA reopen Onboard | **does not** clear skip (see §5.5) |

New client-only intents (mirror `Intent::SetTheme`):

- `Intent::SetOnboardComplete`
- `Intent::SetOnboardSkipped`

Harness drain: mutate `SessionStore` + `save`.

### 4.4 Overlay shell

```rust
enum OnboardStep {
    Triage { selected: usize },
    SkipConfirm { selected: usize },
    // Hosted / Local reuse nested add-model steps OR push existing AddModel*
    // overlays — see §5. Prefer: Onboard triage only; then hand off to
    // enter_add_model_flow / ProviderPicker-equivalent without losing
    // "came from onboard" flag for completion.
    HostedProvider { query: String, selected: usize },
    // ... or: after triage, set Overlay::ProviderPicker / enter_add_model_flow
    // with AppState.onboard_active: bool
}
```

**Recommended architecture (pick one in plan; prefer A):**

- **A (preferred):** `Overlay::Onboard` only for Triage + SkipConfirm. Selecting Hosted/Local/ACP sets `state.onboard_active = true` and calls existing `enter_add_model_flow` / ACP path. On successful `apply_add_model` / `AcpConnected` while `onboard_active`, pin `pending_model`, emit `SetOnboardComplete`, clear flag.
- **B:** Full wizard steps duplicated inside `OnboardStep` — more UI work, higher drift risk.

Esc:

- Triage → SkipConfirm (Skip forever / Continue / Cancel)
- Mid AddModel* → existing Esc back behavior
- SkipConfirm Cancel → Triage

---

## 5. Wizard flows

### 5.1 Triage rows

1. **Hosted (API key)** — catalog hosted, `requires_key`
2. **Local** — Ollama / LM Studio style (`local`, no key)
3. **ACP agent** — ACP provider cards / connect

Footer: Enter · Esc skip · arrows.

### 5.2 Hosted

Reuse today’s flow after triage:

1. Filter providers: hosted + available + can_list (or free-text fallback)
2. Env prefill: if catalog `api_key_env` (or AuthMethod env list) is **set in process env**, treat as `has_key` and skip `AddModelProviderKey` (or show “Using $VAR” confirm — Use / Replace). Prefer skip-to-query when env present (matches product “env prefill”).
3. Blank key **rejected** (§6.3)
4. Query with request generation (§6.2)
5. Pick → `Intent::AddModel` → harness write → **pin `pending_model`** (today `apply_add_model` does **not** pin — onboard must) → complete

### 5.3 Local

Same as Tab/Enter on local provider: query without key → pick → AddModel `api_key: None`.

**OLLAMA_HOST:** probes currently assume catalog default (`localhost:11434`). MVP: clear failure if unreachable; optional fix if Local is otherwise unusable.

### 5.4 ACP

Reuse `/provider` ACP branch:

- Uninstalled → `Intent::AddModel` → `connect_acp_agent`
- Installed → probe / pick
- Success path already pins `pending_model` in `AcpConnected` handler — also fire `SetOnboardComplete` when `onboard_active`

### 5.5 Skip & CTA

Skip forever → prefs + Chat. Empty hero today (`render.rs` ~1622):

> `✦  Connect your first model` … `/` → Provider catalog

Target: Enter (or dedicated hint) on empty hero **reopens `Overlay::Onboard` Triage** without clearing `onboard_skipped`. Optional: keep `/provider` as secondary.

---

## 6. Discovery trust (required same ship)

### 6.1 Env-aware `has_key` + query auth

**Today:** `load_provider_cards` sets

```text
has_key = auth.get(provider/{id}).is_some_and(|k| !k.is_empty())
```

Query key = overlay key ∨ `stored_provider_key(auth.json)` — **no env**.
Runtime `ModelRegistry::api_key_for` **does** resolve env — TUI is inconsistent.

**Target resolution for TUI discovery (align with runtime precedence as far as client can without async):**

1. Non-empty `auth.json[provider/{id}]`
2. Else first non-empty env var from catalog provider `AuthMethod::ApiKey.env` list
3. (Model-specific auth is N/A before model exists)

Also:

- Pass resolved key into `query_provider_models`
- Apply catalog `extra_headers` on the GET when present
- After `apply_add_model`, **reload provider cards** so `has_key` updates (today only reloads models + key statuses)

### 6.2 Request generation

**Today:** `Intent::QueryProviderModels { provider_id, api_key, refresh }`
`Action::ProviderModelsLoaded { provider_id, models, origin }` — match by `provider_id` only.

**Target:** add `request_id: u64` (monotonic per AppState or per provider):

- Intent + Action + Failed carry `request_id`
- Reducer ignores Loaded/Failed if `request_id != latest` for that in-flight overlay
- Soft-fail Catalog must not overwrite a newer Live for a higher request_id
- Do not clear `refreshing` for superseded responses

### 6.3 Blank key guards

| Path | Today | Target |
|------|-------|--------|
| `ApiKeySet` | Reject blank | Keep |
| `AddModelKey` | Blank → `AddModel { api_key: None }` | **Reject** (stay on overlay; optional notice) |
| `AddModelProviderKey` | Blank → query with None | If `requires_key` and no env/auth: **reject**; if env/auth exists, blank may mean “use stored/env” — prefer explicit Use env path |
| Test `add_model_provider_key_blank_queries_with_no_key` | Documents bad behavior | **Rewrite** to assert reject or env-use |

---

## 7. Error handling

- Probe fail → stay in flow with reason + Retry/Back (existing Failed → AddModelId fallback OK for non-onboard; onboard should not dump to empty Chat)
- Write fail → notice; stay
- ACP fail → notice; stay
- Detach/Ctrl-C unchanged

---

## 8. Testing (minimum)

1. Gate: empty + not skipped → Onboard after splash decision helper
2. Skip sticky across SessionStore load/save
3. Splash render/harness: empty runnable → stage string **excludes** `"is ready"`
4. `has_key` true when env set, auth.json empty
5. Stale `ProviderModelsLoaded` older `request_id` ignored
6. `AddModelKey` blank emits **no** Intent
7. Onboard accessible announcement includes triage labels
8. `apply_add_model` / onboard complete sets `pending_model` when `onboard_active`

---

## 9. Acceptance criteria

- [ ] Empty `models.toml`: splash honest; Enter opens Onboard
- [ ] Hosted + env prefill completes without typing key
- [ ] Blank hosted key rejected
- [ ] Local + ACP paths can pin a runnable model
- [ ] Skip forever sticky; CTA reopens Onboard without clearing skip
- [ ] Env keys counted by `has_key` + probes authenticated
- [ ] Stale ProviderModelsLoaded cannot clobber fresher Live
- [ ] No OAuth UI
- [ ] Architecture picker Favorites/fuzzy not required

---

## 10. Suggested implementation order

1. Discovery trust (§6) — unblock wizard
2. SessionStore prefs + runnable helper + splash honesty + post-splash gate
3. `Overlay::Onboard` Triage + SkipConfirm + `onboard_active` handoff
4. Wire Hosted/Local/ACP via existing flows + pin + complete intent
5. Empty CTA + accessible + tests

---

## 11. Open points (resolve in plan, not product debate)

1. Exact serde shape for prefs bools (`Option<bool>` vs `bool` default false) — match `theme` style
2. Seed Setup Issue when models empty — nice-to-have if cheap
3. OLLAMA_HOST — document vs fix
4. Architecture A vs B for Onboard — **prefer A**

---

## 12. Handoff notes

- Do not expand into architecture picker redesign
- Ignore audit REJECTED: approval a/r steal, FOOTER_HINTS dead-code, stream starve framing, named GetArtifact, composer cursor fixed, ScrollArea/grid fixed
- User preference: **ACP > OAuth**

---

## 13. Data-flow diagrams

### Post-splash gate

```text
boot → seed models/providers → splash stage (honest)
     → Enter
     → runnable empty && !onboard_skipped ?
          yes → Overlay::Onboard Triage
          no  → Chat (+ Connect CTA if empty)
```

### Hosted add (reuse)

```text
Triage Hosted → pick provider
  → has_key (auth∪env)? query : AddModelProviderKey
  → QueryProviderModels{request_id}
  → ProviderModelsLoaded (ignore stale id)
  → AddModelPick → Intent::AddModel
  → write_add_model → reload models+providers
  → pending_model = display_id
  → SetOnboardComplete → overlay None
```

---

## 14. Code inventory (current)

| Concern | Location |
|---------|----------|
| Splash render | `crates/tui/src/render.rs` `render_splash` |
| Ready stage always “is ready” | `crates/cli/src/tui.rs` ~360 |
| Splash Enter gate | `wait_for_splash_entry`, `splash_gate_decision` |
| `ModelCard` / readiness | `crates/tui/src/state.rs` |
| Load models | `load_model_cards` in `cli/src/tui.rs` |
| `ProviderCard.has_key` | `load_provider_cards` ~5784 |
| Query GET | `query_provider_models` ~4714 |
| Intent/Action discovery | `crates/tui/src/action.rs` `QueryProviderModels`, `ProviderModelsLoaded` |
| Fold loaded | `reduce.rs` `on_provider_models_loaded` |
| Enter add-model | `enter_add_model_flow` `reduce.rs` |
| Blank AddModelKey | `reduce.rs` ~4287–4308 |
| Write model | `write_add_model` / `apply_add_model` |
| ACP pin | `AcpConnected` handler ~1815 |
| SessionStore | `cli/src/tui.rs` ~6836 |
| Empty Connect hero | `render_conversation` ~1622 |
| Runtime key precedence | `ModelRegistry::api_key_for` `runtime/src/models.rs` ~729 |
| Catalog | `crates/providers/builtin_catalog.toml` |

---

## 15. Spec self-review

- No TBD product forks left (A preferred for Onboard architecture).
- Scope = ship B plus the explicitly approved workbench and curated-learning amendments below.
- Splash “is ready” and runnable gate are explicit and consistent.
- Discovery trust is mandatory, not optional polish.

---

## 16. Competitive workbench UX amendment

The terminal is an agent workbench, not a chat transcript surrounded by unrelated
modal lists. The following requirements are part of the same product contract.

### 16.1 Persistent session strip

The bottom of Chat always presents a width-budgeted projection of:

- active model and provider;
- mode/reasoning posture;
- context used/remaining;
- cost when available;
- permission posture;
- repository branch/worktree state;
- active and queued subagents, including council workers;
- connection/integration health.

On narrow terminals, preserve model, mode, context and active-agent count first;
move lower-priority details into `/status`. A transient notice must not erase
approval, failure, or agent-activity indicators.

### 16.2 Provider and model selection

Provider selection drills into an actual supplier-owned or catalog model list.
The model picker supports filtering, arrows/wheel, PgUp/PgDn, Home/End, current
and staged state, readiness, auth source, context, cost, test/retry, and deletion
of user-configured model profiles. A bare `acp/<agent>` row is a supplier entry,
not a pretend concrete model; Enter opens the ACP model catalogue when supported.

### 16.3 Command palette and composer

- The `/` palette uses responsive two-column rows only when there is room;
  otherwise it uses a single column with non-overlapping title, summary and key.
- Search always preserves the newest query text and caret.
- The composer is at least three visible text rows at normal terminal heights,
  grows by wrapped display rows, preserves its draft, and exposes queued,
  steer-next, interrupt and send states explicitly.
- Multiline input, history and editor handoff remain available without hidden
  state changes behind security or onboarding modals.

### 16.4 Transcript, failure and recovery

Tool calls, diffs, thinking, memory changes, council activity and failures render
as compact expandable cards. A failure names the provider/model, phase and a
sanitized cause and offers applicable recovery actions: Retry, Re-authenticate,
Diagnostics, Choose model, or Disable. Generic `ACP prompt failed` is not an
acceptable terminal state.

### 16.5 Councils and subagents

Every worker has a visible role, provider/model, task, status, elapsed time and
result. The operator can focus, steer, stop, retry and inspect it without leaving
the main conversation. Council creation exposes members, chair, rounds and
synthesis policy; a successful council can be saved as a reusable template.

Council completion is durable and unambiguous: the transcript renders a
completed/failed terminal card, the full chair synthesis is inspectable and
copyable, and the result is persisted as an artifact linked to repository,
session, council definition and participating runs. Later turns retrieve it
through a dedicated council-result projection/tool; they must not guess that it
is a workflow or blackboard entry. A line such as `chair synthesis (92 lines)`
without the result or a durable handle is incomplete behavior.

### 16.6 Clipboard and terminal selection

Users can select and copy transcript, council, diagnostics, memory and document
text with their terminal's native mouse selection. Interactive row hit targets
must not consume drag selection across ordinary text. Clipboard paste inserts
sanitized, bounded text into the active composer/editor, including multiline
content. Explicit copy actions cover the latest assistant response, focused
card, council synthesis and diagnostics for terminals where mouse mode cannot
preserve native selection.

### 16.7 Workflow, Kanban and Blackboard discoverability

The palette and help teach the distinction:

- **Kanban** is the repository task board: create, assign and move task cards.
- **Workflow** is a persisted executable graph: create/open a manifest, run it,
  pause/resume nodes and inspect state.
- **Blackboard** is the evidence/decision/artifact stream produced by workflows
  and agents; operators can inspect and post governed entries.

Each empty state has a primary action and a concrete example. Agent tools expose
the same primitives, and a council-to-project handoff may explicitly create
Kanban tasks or post a result artifact—but it never happens invisibly.

Natural language in Chat is the primary creation surface. Requests such as
“build a release workflow,” “turn this plan into Kanban tasks,” “post this
decision to the blackboard,” and “run a security council with Claude, Codex and
Kimi” resolve to typed domain tool calls. The agent gathers missing fields,
previews the exact graph/cards/entry/council, requests the existing policy/user
confirmation when execution or mutation requires it, applies the operation,
and links the resulting durable object back into the transcript. It must not
respond with instructions to open the TUI surface when it has authority and the
typed tool needed to complete the request.

### 16.8 Accessibility and responsive behavior

Every graphical action has a cooked-mode semantic equivalent. Modal focus is
trapped without mutating hidden state. Mouse hit targets match visible rows.
Golden coverage includes 40/60/80/120+ column terminals, monochrome/high-contrast,
long provider/model/command strings and keyboard-only navigation.

---

## 17. Curated learning loop amendment

Growth means retaining a small amount of high-value knowledge that improves a
future task. It never means copying every turn or run completion into memory.

### 17.1 Knowledge classes

| Class | Purpose | Load policy |
|-------|---------|-------------|
| User profile | Stable preferences and communication/workflow choices | Small, cross-project |
| Repository fact | Verified conventions, architecture and environment facts | Repository-scoped |
| Provider lesson | Auth, model and adapter quirks verified locally | Provider-scoped |
| Council recipe | Roles, models, rounds and synthesis pattern | On demand |
| Skill | Reusable, ordered procedure with prerequisites and verification | On demand |
| Episode | Searchable provenance/history, not injected by default | Never automatic |

### 17.2 Capture policy

Automatic proposals may be produced only from direct user preferences,
corrections, verified repository facts, or successful locally-observed outcomes.
Greetings, generic completions, raw logs, temporary paths, secrets, external
instructions and untrusted tool/web content are rejected. Inference is a
proposal, never active authority.

Every item carries scope, provenance, confidence, lifecycle state, creation and
verification timestamps, optional expiry, and supersession links. Dedupe and
conflict handling are deterministic and bounded.

### 17.3 Learning UX

- A run may show a quiet `Learned N useful things` summary with Undo.
- `/journey` shows the chronological learning ledger and why an item exists.
- `/memory` permits inspect, edit, approve/reject, pin, supersede and delete.
- `/skills` distinguishes installed, agent-authored, stale and verified skills.
- A complex successful workflow may propose a skill containing steps,
  prerequisites, verification commands and discovered pitfalls.
- Recalled learning displays a compact `why used` explanation.

### 17.4 Security and quality

Learning cannot grant tools, permissions, credentials or command authority.
Untrusted provenance cannot auto-activate. Sensitive content is rejected or
redacted before persistence. Repository/user/profile isolation is enforced at
the query boundary. Tests cover memory poisoning, stale/conflicting facts,
bounded retention, delete/forget and backwards-compatible migration.
