# Adoption 19 — Plan mode

**Effort:** M · **Depends on:** nothing (composes over shipped adoptions 03 questions + 06 prompt queue) · **Reference:** reference-repos/opencode/packages/opencode/src/tool/plan.ts, src/agent/agent.ts, src/session/session.ts (`plan()`), src/session/prompt/plan-mode.txt, src/session/prompt/plan.txt, src/session/prompt/build-switch.txt, src/tool/plan-exit.txt, src/tool/plan-enter.txt
**Ported from:** opencode · **Status:** ⬜ not started

## 1. Summary

Turn the existing **Plan** mode from a read-only prompt overlay into a real, policy-enforced *plan-and-hand-off* loop, composed entirely over machinery that already ships — the deny-wins policy engine, the mode picker, the question tool (adoption 03), and the durable prompt queue (adoption 06). Two actions:

- **Action 15 — Plan profile with policy teeth.** Today `AgentMode::Plan` denies **every** filesystem write (including the plan file). Give Plan exactly one write exception — a file under the run's plan directory `<worktree>/.codypendent/plans/` — while every other write stays denied, enforced in `crates/daemon/src/policy/`, not in a tool. The model can therefore author its plan with `workspace.write_file`/`workspace.edit_file` and nothing else.
- **Action 16 — `plan_enter` / `plan_exit` bridge tools + synthetic approval.** `plan_enter` is a **Build-mode** tool ("if the user asks for a plan, call this first") that offers switching into Plan; `plan_exit` is a **Plan-mode** tool that asks the operator "plan complete — switch to Build?" Both use the **shipped question tool** for the yes/no, and on "yes" **synthesize a follow-up user message** (via the shipped prompt-queue seam) that records the mode switch in-band and runs the next turn in the target mode. `plan_exit`'s reject path reuses adoption 03's reject-with-feedback (the model treats "no, keep planning because …" as steering).

No new migration is required: Action 15 is pure policy (in-memory evaluation context), and Action 16 reuses the `questions` table (migration 0034) and the `pending_prompts` table (migration 0037). The next free migration number, `0039`, stays free.

## 2. Reference implementation

All paths under `reference-repos/opencode/`.

**The plan agent's ruleset** (`packages/opencode/src/agent/agent.ts` lines 156-181). Plan is a *primary agent* (selected like `build`), default-allow, but overridden so that **all edits are denied except the plan-file globs**:

```ts
plan: {
  name: "plan",
  description: "Plan mode. Disallows all edit tools.",
  permission: Permission.merge(defaults, Permission.fromConfig({
    question: "allow",
    plan_exit: "allow",
    task: { general: "deny" },
    external_directory: { [path.join(Global.Path.data, "plans", "*")]: "allow" },
    edit: {
      "*": "deny",
      [path.join(".opencode", "plans", "*.md")]: "allow",
      [path.relative(ctx.worktree, path.join(Global.Path.data, "plans", "*.md"))]: "allow",
    },
  }), user),
  mode: "primary",
},
```

The `defaults` set `plan_enter: "deny"`, `plan_exit: "deny"`, `question: "deny"` everywhere; the **build** agent merges `{ question: "allow", plan_enter: "allow" }`. So `plan_enter` is a build-only gate, `plan_exit` a plan-only gate. **`edit "*" = deny` with a plan-file allow-exception is the exact shape Action 15 ports** into the deny-wins engine.

**Plan-file path** (`packages/opencode/src/session/session.ts` lines 331-336): `<base>/<created_ts>-<slug>.md`, `base = <worktree>/.opencode/plans` (VCS) else the global data dir. Codypendent's port uses `<worktree>/.codypendent/plans/`, matching the established `.codypendent/` convention (`crates/daemon/src/hooks.rs` `<repo>/.codypendent/hooks`, `crates/daemon/src/policy/config.rs` `<repo>/.codypendent/policy.toml`).

**`plan_exit`** (`packages/opencode/src/tool/plan.ts`, 79 lines) — the ONLY implemented plan tool. Empty params. Execute:
1. Compute the plan path relative to the worktree.
2. `question.ask({ question: `Plan at ${plan} is complete. Would you like to switch to the build agent and start implementing?`, header: "Build Agent", custom: false, options: [{label:"Yes",…},{label:"No",…}] })`.
3. `"No"` → `Question.RejectedError` (stays in plan).
4. `"Yes"` → **synthesize a new user message** with `agent: "build"` plus a synthetic text part `The plan at ${plan} has been approved, you can now edit files. Execute the plan`; return `{ title: "Switching to build agent", output: "User approved switching to build agent. Wait for further instructions." }`. The new message's `agent: "build"` field is what flips the next turn into build mode.

**`plan_enter`** (`packages/opencode/src/tool/plan-enter.txt`) — the prompt text exists but **opencode ships no tool implementation**; `plan_enter` is only a vestigial permission name. Codypendent **does** implement it (the task calls for both bridge tools), modelled symmetrically on `plan_exit`: a build-mode tool that asks "switch to plan first?" and, on yes, synthesizes a Plan continuation carrying the request to plan.

**Prompt discipline** (`packages/opencode/src/session/prompt/plan-mode.txt`, `plan.txt`, `build-switch.txt`, `tool/plan-exit.txt`, `tool/plan-enter.txt`) — quoted where ported in §5.7/§5.8. The load-bearing lines: *"you MUST NOT make any edits (with the exception of the plan file mentioned below) … this is the only file you are allowed to edit"*; *"your turn should only end with either asking the user a question or calling plan_exit"*; and the build-switch reminder *"Your operational mode has changed from plan to build. You are no longer in read-only mode."*

## 3. Current state in codypendent (verified — what Plan mode does TODAY)

- **`AgentMode::Plan` exists** (`crates/protocol/src/run.rs` line 24) with the doc "Produce an execution plan. May write plan artifacts only." — but the "may write plan artifacts" promise is **not yet kept by policy**.
- **`mode_overlay(Plan)`** (`crates/runtime/src/agent.rs` lines 1343-1366) returns `ModeOverlay { write_allowed: false, command_allowed: true, network_allowed: false }`. All writes denied; safe commands allowed; network denied.
- **`ModeOverlay`** (`crates/daemon/src/policy/mod.rs` lines 120-155) is a `Copy` three-bool struct; `EvalContext` (lines 159-181) carries `repository`, `worktree`, `mode`. **Neither carries a path**: `evaluate` dispatches `ProposedAction::WritePatch { .. } => self.eval_write(ctx)` (line 319) and `eval_write` (lines 556-577) ignores the target path entirely — it early-returns `Deny` on `!ctx.mode.write_allowed` (line 557), then checks the merged write scope. **So in Plan mode every write, including the plan file, is denied with `policy.write-denied-by-mode`.**
- **`WritePatch` carries only an `ArtifactId`, never a path** (`crates/protocol/src/run.rs` lines 91-93). `workspace.write_file`, `workspace.edit_file`, and `git.apply_patch` all lower to `ProposedAction::WritePatch { patch: stored.id }` (`crates/runtime/src/agent.rs` lines 4118-4181); the resolved target lives in the typed `PreparedTool::WriteFile(input)` / `EditFile(input)` (`input.path`), which the policy engine never sees. **This is why the plan-file exception must thread the write target into evaluation** — the wire action cannot carry it.
- **`offered_tool_names` already drops the write tools in Plan mode** (`crates/runtime/src/agent.rs` lines 2157-2166): `if !overlay.write_allowed && matches!(name, WriteFile::NAME | EditFile::NAME | ApplyPatch::NAME) { return false }`. So today a Plan run is never even offered a way to write the plan. **Action 15 must re-offer the plan-file writers in Plan mode.**
- **Plan-mode prompt already seeded**: `PLAN_MODE_INSTRUCTION` (`crates/runtime/src/agent.rs` lines 178-185) is prepended to a Plan run's objective via `mode_seed_instruction` (lines 218-225). It currently tells the model "do NOT attempt to write, edit, or patch any files" — which contradicts the new plan-file exception and **must be updated** to "write your plan to the plan file, the only file you may edit."
- **The question tool ships** (adoption 03): `crates/runtime/src/tools/question.rs`, `crates/daemon/src/questions.rs` (`QuestionBroker`), the `QuestionChannel` runtime seam on `AgentRuntime` (`with_questions`), `CommandBody::ResolveQuestion`, and the reject-with-feedback path (`QuestionOutcome::Rejected { feedback }`). Adoption 03 §5 note 3 and §10 explicitly reserve the internal `custom: false` caller "e.g. a plan-exit flow" — **this spec is that caller.**
- **The prompt queue ships** (adoption 06): `crates/daemon/src/prompt_queue.rs` (`enqueue`/`shift_next`/…), the `PendingPromptsChanged` event, `CommandBody::QueuePrompt { session_id, text, mode, delivery }`, and the `PromptQueueDrainer` that launches `SubmitUserInput` continuations one at a time when the session goes idle. **A queued prompt carries its own `AgentMode`** (`PendingPromptView.mode`, `pending_prompts.mode` column) — exactly the seam Action 16 needs to launch the next turn in a different mode.
- **`SubmitUserInput` carries `mode: AgentMode`** (`crates/protocol/src/command.rs` lines 142-175) — the continuation's mode is per-command, so a mode switch needs no session-level persisted state.
- **The scope machinery** (`crates/daemon/src/policy/scope.rs`): `PathScope { roots, deny }` with `classify`/`resolve` doing canonicalize-then-component-wise containment, deny-wins. Reused verbatim for the plan-file containment check.
- **Must not break**: the deny-wins ordering in `eval_write`/`eval_command` (the plan exception only ever *adds* a narrow allow, never widens an existing deny); the `WritePatch` wire shape and its golden vector (`run.rs` line 529 — unchanged, no path added to the wire); `offered_tool_names`' invariant that it "only ever REMOVES a name the overlay's policy evaluation would deny" (the plan-mode re-offer is sound precisely because policy now *allows* the plan-file write); the question-tool parking contract and the prompt-queue drain rules.

## 4. Design

### 4.1 Action 15 — the plan-file write exception, in the policy engine

The write target is threaded into evaluation through **`EvalContext`** (an internal, non-wire type), keeping `WritePatch` byte-identical on the wire and keeping the *decision* inside the deny-wins engine (never in a tool):

1. `EvalContext` gains two fields (both default-`None`, so every existing call site compiles unchanged):
   - `plan_write_root: Option<PathBuf>` — `Some(<worktree>/.codypendent/plans)` **only** for a Plan-mode run.
   - `write_target: Option<PathBuf>` — the resolved absolute path of the write under evaluation, populated per tool-call for `workspace.write_file`/`workspace.edit_file`.
2. `eval_write` gains the exception at the top of its existing `!write_allowed` deny branch: if `plan_write_root` and `write_target` are both `Some` and the target is inside the plan root (checked via a `PathScope` built from the plan root, so canonicalization + deny-wins + symlink-escape guards are reused), return `Allow(FileWrite(plan_scope))` with a new reason `policy.plan-file-write-allowed`. Otherwise the branch denies exactly as today.
3. `mode_overlay(Plan)` keeps `write_allowed: false` — the exception is *additive on top of* the mode denial, so a Plan run's git-shell mutations (`eval_command` line 627), `git.apply_patch` (no single `write_target`), and worktree writes all stay denied. Only a `write_target` under the plan directory is allowed.
4. `offered_tool_names` re-offers `workspace.write_file` and `workspace.edit_file` (the plan-file writers) in Plan mode so the model can actually author the plan; `git.apply_patch` stays dropped (a multi-file diff can't be scoped to the plan directory, and the plan is authored file-by-file anyway).
5. `PLAN_MODE_INSTRUCTION` is rewritten to tell the model the plan file is the one writable target.

This is deny-wins preserving by construction: the only new code path is an `Allow` *inside* a branch that otherwise denies, gated on a path being under one specific directory.

### 4.2 Action 16 — bridge tools + synthetic mode switch

Two always-allowed tools (`ProposedAction::PlanTransition { target }`, evaluated like `AskUser`), each gated by run mode and by two wired seams:

- **`plan_enter`** — offered only when `run.mode == Build` and both the `QuestionChannel` (adoption 03) and a new `PlanBridge` seam are wired. Asks "This looks like it would benefit from planning first. Switch to Plan mode to research and design before editing?" (Yes/No, `custom: false`). On **Yes**, enqueue a **Plan** continuation carrying the request to plan; return "Switching to plan mode. Wait for further instructions." On **No/reject**, return an observation telling the model to proceed in Build.
- **`plan_exit`** — offered only when `run.mode == Plan` and both seams are wired. Asks "Plan at `<plan-file>` is complete. Switch to the Build agent and start implementing?" (Yes/No, `custom: false`). On **Yes**, enqueue a **Build** continuation whose synthetic text is `The plan at <plan-file> has been approved, you can now edit files. Execute the plan.`; return "User approved switching to build agent. Wait for further instructions." On **No**, return "Staying in Plan mode — continue refining the plan." On **reject-with-feedback**, fold the feedback into the observation as a correction (the shipped adoption 03 path).

**The synthetic message** is an enqueue on the shipped prompt queue (adoption 06), not a bespoke channel: a `QueuePrompt`-equivalent with `delivery: Queue` and `mode: <target>`. Because the current run is still active when the tool returns, the queue entry drains via the existing `PromptQueueDrainer` the moment the current run reaches `Completed` — launching the continuation in the target mode as a real, ledgered `SubmitUserInput`. That is the "records the switch in-band" property: the switch appears as a `PendingPromptsChanged` event and then a genuine command in the ledger, replayable and recoverable, never a hidden state flip.

The tools first **park** on the question (transitioning the run to `WaitingForUserInput`, adoption 03's machinery) and resume on the answer, exactly like `user.ask`; the enqueue happens after the answer, before the tool returns its observation.

## 5. Changes, file by file

Literal Rust below is normative for names, reason codes, and observation strings.

### 5.1 `crates/daemon/src/policy/mod.rs` — the plan-file exception

Extend `EvalContext` (after the `mode` field, lines 159-181):

```rust
pub struct EvalContext {
    pub repository: PathBuf,
    pub worktree: PathBuf,
    pub mode: ModeOverlay,
    /// The one directory a `write_allowed == false` mode still permits writes
    /// under (adoption 19, Plan mode's `<worktree>/.codypendent/plans`).
    /// `None` for every mode without a plan-file exception. A write is allowed
    /// by this exception ONLY when [`write_target`](Self::write_target) resolves
    /// inside this root — deny-wins is untouched: this can never widen a deny
    /// that the file policy or another overlay imposes.
    pub plan_write_root: Option<PathBuf>,
    /// The resolved absolute path of the write under evaluation, when the
    /// caller knows it (the `workspace.write_file` / `workspace.edit_file`
    /// target). `None` for non-write actions and for multi-file patches, which
    /// therefore never qualify for the plan-file exception.
    pub write_target: Option<PathBuf>,
}
```

Update `EvalContext::new` to default both to `None`, and add builders:

```rust
    /// Set the plan-file write root (adoption 19). See [`plan_write_root`].
    #[must_use]
    pub fn with_plan_write_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.plan_write_root = Some(root.into());
        self
    }

    /// Set the resolved write target for this evaluation (adoption 19).
    #[must_use]
    pub fn with_write_target(mut self, target: impl Into<PathBuf>) -> Self {
        self.write_target = Some(target.into());
        self
    }
```

Rewrite `eval_write` (lines 556-577) so the exception lives inside the existing mode-deny branch:

```rust
    fn eval_write(&self, ctx: &EvalContext) -> PolicyDecision {
        if !ctx.mode.write_allowed {
            // Adoption 19 — Plan mode's single write exception: a write whose
            // resolved target is inside the run's plan directory is permitted
            // even though the mode otherwise forbids all writes. Everything
            // else stays denied. Deny-wins is preserved: this only ADDS an
            // allow inside a branch that would otherwise deny, and the grant is
            // scoped to exactly the plan directory.
            if let (Some(plan_root), Some(target)) =
                (ctx.plan_write_root.as_deref(), ctx.write_target.as_deref())
            {
                let plan_scope = PathScope::new(vec![plan_root.to_path_buf()], Vec::new());
                if plan_scope.allows(target) {
                    return self.allow(
                        Capability::FileWrite(plan_scope),
                        PolicyReason::new(
                            "policy.plan-file-write-allowed",
                            "Plan mode permits writing the plan file under \
                             .codypendent/plans/",
                        ),
                    );
                }
            }
            return self.deny(PolicyReason::new(
                "policy.write-denied-by-mode",
                "the active mode forbids filesystem writes",
            ));
        }
        let scope = self.file_write_scope(ctx);
        if scope.roots.is_empty() {
            return self.deny(PolicyReason::new(
                "policy.no-write-scope",
                "no writable roots are in scope",
            ));
        }
        self.allow(
            Capability::FileWrite(scope),
            PolicyReason::new(
                "policy.write-allowed",
                "writes are permitted within the worktree scope",
            ),
        )
    }
```

Add an always-allow arm for the new bridge action in `evaluate` (next to `AskUser`, line 380):

```rust
            ProposedAction::PlanTransition { .. } => self.eval_plan_transition(),
```

```rust
    /// A `plan_enter`/`plan_exit` call (adoption 19) is always permitted: like
    /// `user.ask` it targets only the session's own human (a yes/no question)
    /// and the session's own durable prompt queue — no filesystem, command,
    /// network, or remote effect — so it grants no capability and never reaches
    /// the approval gate.
    fn eval_plan_transition(&self) -> PolicyDecision {
        PolicyDecision {
            decision: Decision::Allow,
            reasons: vec![PolicyReason::new(
                "policy.plan-transition-allowed",
                "a plan-mode transition targets only the session's own human and queue",
            )],
            capability_grant: None,
            policy_version: self.version.clone(),
            approval_reusable: false,
        }
    }
```

### 5.2 `crates/protocol/src/run.rs` — the transition action

Append to `ProposedAction` (before `#[serde(other)] Unknown`):

```rust
    /// Offer the operator a mode switch and, on their approval, enqueue the
    /// next turn in `target` (adoption 19: `plan_enter` / `plan_exit`). Targets
    /// only the session's own human (a yes/no question) and its own prompt
    /// queue — no filesystem, command, network, or remote effect — so it is
    /// always policy-`Allow`ed like [`Self::AskUser`] and recorded purely so the
    /// transition offer is traced. Never serialized into a `ToolProposed`.
    PlanTransition {
        /// The mode the accepted continuation runs in (`Plan` from
        /// `plan_enter`, `Build` from `plan_exit`).
        target: AgentMode,
    },
```

Add a round-trip line to the existing `ProposedAction` test (near line 529): `round_trip(ProposedAction::PlanTransition { target: AgentMode::Build });`.

### 5.3 `crates/runtime/src/agent.rs` — the plan directory + seam + eval wiring

**Plan directory helper** (near `mode_overlay`):

```rust
/// The one directory a Plan run may write (adoption 19). Lives under the run's
/// worktree so a later Build continuation in the same session reads it back,
/// and under the established `.codypendent/` convention. Returned only for
/// `AgentMode::Plan`; `None` disables the policy exception for every other mode.
pub fn plan_write_root(mode: AgentMode, worktree: &Path) -> Option<PathBuf> {
    matches!(mode, AgentMode::Plan)
        .then(|| worktree.join(".codypendent").join("plans"))
}
```

**`eval_ctx` gains the plan root** (lines 6038-6044):

```rust
    fn eval_ctx(&self, run: &RunContext) -> EvalContext {
        let mut ctx = EvalContext {
            repository: run.read_root.clone(),
            worktree: run.worktree.clone(),
            mode: mode_overlay(run.mode),
            plan_write_root: None,
            write_target: None,
        };
        if let Some(root) = plan_write_root(run.mode, &run.worktree) {
            ctx = ctx.with_plan_write_root(root);
        }
        ctx
    }
```

**Populate `write_target` at the policy call** (`run_tool`, line 3778). Add a small helper and thread it in:

```rust
/// The resolved absolute write target of a prepared tool, when it targets a
/// single file (`workspace.write_file` / `workspace.edit_file`). Resolved
/// against the worktree so a relative model path is comparable to the plan
/// root; multi-file patches and non-write tools return `None` and so never
/// qualify for the plan-file exception (adoption 19).
fn prepared_write_target(worktree: &Path, tool: &PreparedTool) -> Option<PathBuf> {
    let path = match tool {
        PreparedTool::WriteFile(input) => &input.path,
        PreparedTool::EditFile(input) => &input.path,
        _ => return None,
    };
    Some(if path.is_absolute() {
        path.clone()
    } else {
        worktree.join(path)
    })
}
```

```rust
        // (b) evaluate policy under the mode overlay, threading the write
        // target so Plan mode's plan-file exception (adoption 19) can classify
        // it. Non-write tools leave it `None` and are unaffected.
        let mut ctx = self.eval_ctx(run);
        if let Some(target) = prepared_write_target(&run.worktree, &prepared.tool) {
            ctx = ctx.with_write_target(target);
        }
        let decision = self.policy.evaluate(&prepared.action, &ctx);
```

**Re-offer the plan-file writers in Plan mode** (`offered_tool_names`, lines 2157-2166):

```rust
        let overlay = mode_overlay(run.mode);
        let plan_mode = matches!(run.mode, AgentMode::Plan);
        names.retain(|name| {
            if !overlay.write_allowed {
                let is_write =
                    matches!(name.as_str(), WriteFile::NAME | EditFile::NAME | ApplyPatch::NAME);
                // Plan mode keeps the plan-file writers: the policy engine
                // allows a write ONLY under `.codypendent/plans/` and denies
                // everything else, so offering these lets the model author the
                // plan while a stray write to the worktree is still refused.
                let plan_writer =
                    plan_mode && matches!(name.as_str(), WriteFile::NAME | EditFile::NAME);
                if is_write && !plan_writer {
                    return false;
                }
            }
            // ... command_allowed / network_allowed filters unchanged ...
            true
        });
```

**Rewrite `PLAN_MODE_INSTRUCTION`** (lines 178-185) — the plan file is now the model's one write target (ports `plan-mode.txt`):

```rust
const PLAN_MODE_INSTRUCTION: &str = "\
You are running in PLAN MODE. Investigate the request read-only using the \
available tools (read files, search the workspace, run safe read-only \
commands). You MUST NOT edit any file in the workspace or run any \
non-read-only command — those actions are denied in this mode — WITH ONE \
EXCEPTION: your plan file under `.codypendent/plans/`. Build your plan by \
writing to or editing a Markdown file there (create it with \
`workspace.write_file`, refine it with `workspace.edit_file`); it is the only \
file you may write. Use `user.ask` to clarify requirements. When the plan is \
complete, call `plan_exit` to ask the operator to switch to Build and execute \
it. Your turn should end only by asking a question or calling `plan_exit`.";
```

**The `PlanBridge` seam** (next to the `QuestionChannel` trait added by adoption 03):

```rust
/// Pool-erased mode-switch injection (adoption 19), mirroring the
/// `QuestionChannel` seam: the daemon implements it over the shipped prompt
/// queue (adoption 06). `plan_enter`/`plan_exit` call it after the operator
/// approves the switch; the queued continuation drains into a real
/// `SubmitUserInput` when the current run terminates, so the switch is recorded
/// in-band (a `PendingPromptsChanged` event, then a ledgered command) rather
/// than flipping hidden state.
#[async_trait]
pub trait PlanBridge: Send + Sync {
    /// Enqueue a follow-up turn on `session_id`'s prompt queue with the given
    /// mode and synthetic user text (`delivery = Queue`). Idempotent on
    /// `run_id`: a duplicate call for the same transition enqueues once.
    async fn switch_mode(
        &self,
        session_id: SessionId,
        run_id: RunId,
        target: AgentMode,
        text: String,
    ) -> anyhow::Result<()>;
}
```

**Runtime field + builder** (pattern: `questions`):

```rust
    /// The mode-switch channel the plan bridge tools enqueue on (adoption 19),
    /// if wired. `None` (workflow nodes, webhook runs) leaves `plan_enter` /
    /// `plan_exit` unoffered — a switch nobody can drive must not be offerable.
    plan_bridge: Option<Arc<dyn PlanBridge>>,
```

```rust
    #[must_use]
    pub fn with_plan_bridge(mut self, plan_bridge: Arc<dyn PlanBridge>) -> Self {
        self.plan_bridge = Some(plan_bridge);
        self
    }
```

**Tool definitions** in `static_tool_definitions()` (both no-parameter; `plan_enter` takes one optional `request`):

```rust
        decl(
            PlanEnter::NAME,
            "Offer to switch into PLAN MODE before implementing. Call this FIRST \
             when the user asks for a plan, or when the request is complex enough \
             that researching and designing read-only before editing would help. \
             It asks the user whether to switch; on yes, the next turn runs in \
             Plan mode. Do not call it for simple, direct edits the user asked you \
             to just make.",
            json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "The task to plan (defaults to the current objective)"
                    }
                }
            }),
        ),
        decl(
            PlanExit::NAME,
            "Finish PLAN MODE and offer to switch to BUILD to implement. Call this \
             only after you have written a complete plan to the plan file and \
             resolved your open questions. It asks the user whether to switch to \
             Build; on yes, the next turn implements the plan. Do not call it \
             before the plan file is finalized.",
            json!({ "type": "object", "properties": {} }),
        ),
```

Gate them in `offered_tool_names` (before the overlay `retain`, alongside the artifact/question gating): offer `plan_enter` only when `self.plan_bridge.is_some() && self.questions.is_some() && run.mode == AgentMode::Build`; offer `plan_exit` only when both seams are wired and `run.mode == AgentMode::Plan`. Add both to `ALWAYS_ADVERTISED_TOOLS` so the retrieval funnel can never hide them from a run that is offered them.

**`prepare()` arms:**

```rust
            PlanEnter::NAME => {
                let request = args
                    .get("request")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                Ok(Prepared {
                    action: ProposedAction::PlanTransition { target: AgentMode::Plan },
                    tool: PreparedTool::PlanEnter(request),
                })
            }
            PlanExit::NAME => Ok(Prepared {
                action: ProposedAction::PlanTransition { target: AgentMode::Build },
                tool: PreparedTool::PlanExit,
            }),
```

**Execution** — because these tools park (on the question) rather than execute, handle them in `run_tool` after the policy `Allow` (always `Allow`), before the `execute_prepared` tail, structurally like adoption 03's `AskUser` parking block. A single shared helper drives both (they differ only in the target mode, the question wording, and the synthetic text):

```rust
        if let PreparedTool::PlanEnter(_) | PreparedTool::PlanExit = &prepared.tool {
            return self
                .run_plan_transition(run, run_actor, tool, &prepared.tool, actions, cancel)
                .await;
        }
```

```rust
    /// Drive a `plan_enter`/`plan_exit` transition (adoption 19): ask the
    /// operator via the shipped question tool, and on approval enqueue the next
    /// turn in the target mode via the plan bridge. Parks the run in
    /// `WaitingForUserInput` while the question is open, exactly like `user.ask`.
    async fn run_plan_transition(
        &self,
        run: &RunContext,
        run_actor: &Actor,
        tool: &str,
        prepared: &PreparedTool,
        actions: &mut Vec<Value>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<ToolFlow> {
        // Both seams are guaranteed by the advertisement gate; fail legibly if
        // a future caller wires only one.
        let (Some(questions), Some(bridge)) = (&self.questions, &self.plan_bridge) else {
            return Ok(ToolFlow::Observation {
                observation: "plan transitions are not available in this run".to_string(),
                artifact: None,
            });
        };

        let (target, header, prompt, on_yes_text, on_yes_output, on_no_output, plan_path) =
            match prepared {
                PreparedTool::PlanEnter(request) => {
                    let request = request
                        .clone()
                        .unwrap_or_else(|| run.objective.clone());
                    (
                        AgentMode::Plan,
                        "Plan Mode".to_string(),
                        "This request would benefit from planning first. Switch to \
                         Plan mode to research and design (read-only) before editing?"
                            .to_string(),
                        request,
                        "Switching to plan mode. Wait for further instructions.".to_string(),
                        "Staying in Build mode — proceed with the implementation.".to_string(),
                        None,
                    )
                }
                PreparedTool::PlanExit => {
                    let plan_path = latest_plan_file(&run.worktree);
                    let shown = plan_path
                        .as_deref()
                        .map(display_relative(&run.worktree))
                        .unwrap_or_else(|| ".codypendent/plans/".to_string());
                    (
                        AgentMode::Build,
                        "Build Agent".to_string(),
                        format!(
                            "Plan at {shown} is complete. Switch to the Build agent \
                             and start implementing?"
                        ),
                        format!(
                            "The plan at {shown} has been approved, you can now edit \
                             files. Execute the plan."
                        ),
                        "User approved switching to build agent. Wait for further \
                         instructions."
                            .to_string(),
                        "Staying in Plan mode — continue refining the plan.".to_string(),
                        plan_path,
                    )
                }
                _ => unreachable!("run_plan_transition only handles plan tools"),
            };
        let _ = plan_path;

        // The one internal caller of the question service with custom = false
        // (adoption 03 §10): a strict yes/no, no free-text row.
        let question = QuestionPrompt {
            question: prompt,
            header,
            options: vec![
                QuestionOption { label: "Yes".to_string(), description: String::new() },
                QuestionOption { label: "No".to_string(), description: String::new() },
            ],
            multiple: false,
            custom: false,
        };
        let question_id = questions
            .ask(run.session_id, run.run_id, vec![question])
            .await?;
        self.transition(run.session_id, run.run_id, RunState::WaitingForUserInput)
            .await?;
        let reply = tokio::select! {
            reply = questions.await_reply(question_id) => reply?,
            _ = cancel.cancelled() => {
                questions.forget(question_id);
                return Ok(ToolFlow::Cancelled);
            }
        };
        self.transition(run.session_id, run.run_id, RunState::Running).await?;

        let observation = match reply {
            QuestionReplyWire::Answered(answers)
                if answers.first().is_some_and(|a| a.iter().any(|l| l == "Yes")) =>
            {
                bridge
                    .switch_mode(run.session_id, run.run_id, target, on_yes_text)
                    .await?;
                on_yes_output
            }
            QuestionReplyWire::Answered(_) => on_no_output,
            QuestionReplyWire::Rejected { feedback: Some(feedback) } => format!(
                "plan transition declined: the user said: {feedback}. Treat this as \
                 a correction and continue in the current mode; do not re-ask."
            ),
            QuestionReplyWire::Rejected { feedback: None } => on_no_output,
        };
        actions.push(action_digest(tool, "completed", None));
        // ToolStarted/ToolCompleted emitted around the park, as the AskUser arm does.
        Ok(ToolFlow::Observation { observation, artifact: None })
    }
```

Add small helpers `latest_plan_file(worktree) -> Option<PathBuf>` (newest `*.md` under `<worktree>/.codypendent/plans/`, or `None`) and a `display_relative` formatter. Extend `observation_is_refusal` is **not** needed — a declined transition is not a refusal to steer against.

Add `PlanEnter`/`PlanExit` marker types and `PreparedTool` variants:

```rust
    PlanEnter(Option<String>),
    PlanExit,
```

### 5.4 `crates/runtime/src/tools/plan.rs` (new)

```rust
//! The plan bridge tools (adoption 19): `plan_enter` (Build→Plan) and
//! `plan_exit` (Plan→Build). Both ask the operator via the shipped question
//! tool and, on approval, enqueue the next turn in the target mode.

pub struct PlanEnter;
impl PlanEnter {
    pub const NAME: &'static str = "plan_enter";
}

pub struct PlanExit;
impl PlanExit {
    pub const NAME: &'static str = "plan_exit";
}
```

Export from `crates/runtime/src/tools/mod.rs`: `pub use plan::{PlanEnter, PlanExit};`.

### 5.5 `crates/codypendentd/src/executor.rs` — wire the `PlanBridge`

Where the `QuestionChannel` (`PoolQuestionChannel`) is constructed and attached **only for interactive session runs**, construct and attach a `PlanBridge` over the same pool + the shipped prompt-queue machinery + the drainer:

```rust
struct PoolPlanBridge {
    pool: SqlitePool,
    commands: CommandProcessor,   // the same handle the drainer synthesizes SubmitUserInput through
    drainer: PromptQueueDrainer,
    principal: Principal,          // the daemon's own principal, as adoption 06's drain uses
}

#[async_trait]
impl PlanBridge for PoolPlanBridge {
    async fn switch_mode(
        &self,
        session_id: SessionId,
        run_id: RunId,
        target: AgentMode,
        text: String,
    ) -> anyhow::Result<()> {
        // Enqueue exactly as a client's QueuePrompt would, but daemon-driven and
        // idempotent on the run: one transition -> one queued continuation. Reuse
        // the CommandProcessor so the enqueue is durable, ledgered
        // (PendingPromptsChanged), and replay-safe (adoption 06). Then wake the
        // drainer so it launches when the current run reaches Completed.
        let command = CommandBody::QueuePrompt {
            session_id,
            text,
            mode: target,
            delivery: PromptDelivery::Queue,
        };
        self.commands
            .apply_with_idempotency(
                &self.pool,
                &self.principal,
                command,
                format!("plan-switch:{run_id}"),
            )
            .await?;
        self.drainer.notify(session_id);
        Ok(())
    }
}
```

(If `CommandProcessor` is not directly reachable from the executor, mirror adoption 06's own drain call site — it already synthesizes `SubmitUserInput` through the same seam; `switch_mode` synthesizes `QueuePrompt` through it.)

Wire it onto the interactive runtime only: `runtime = runtime.with_plan_bridge(Arc::new(PoolPlanBridge { … }))` in the same block that calls `with_questions`, and never in `workflow_exec.rs`.

### 5.6 Prompt-discipline constants (ported text)

Add a `BUILD_SWITCH_NOTE` prepended to the seeded objective of a **Build continuation that follows a Plan turn**, so the model knows the mode changed (ports `build-switch.txt`). Detecting "the previous turn was Plan" is available from the queued entry: the synthetic `on_yes_text` already carries "The plan at … has been approved, you can now edit files. Execute the plan." — that text is the in-band build-switch signal and is sufficient; a separate reminder is optional and, if added, is seeded (never ledgered) like `PLAN_MODE_INSTRUCTION`.

## 6. Protocol & persistence

- **New wire type**: `ProposedAction::PlanTransition { target: AgentMode }` — internally tagged, `#[non_exhaustive]` already on the enum, defaulted forward-compat via the enum's `#[serde(other)] Unknown`. It is always policy-`Allow`ed and never serialized into a `ToolProposed`, so it needs no golden approval vector — only the `run.rs` round-trip line (§5.2).
- **Reused commands/events**: `CommandBody::QueuePrompt` and `EventBody::PendingPromptsChanged` (adoption 06) carry the switch; `CommandBody::ResolveQuestion` and `EventBody::QuestionAsked/QuestionResolved` (adoption 03) carry the confirmation. No new command or event.
- **Run state**: `Running → WaitingForUserInput → Running` around the confirmation question — transitions already legal (`crates/daemon/src/ledger.rs`), driven by adoption 03's machinery. No ledger change.
- **Persistence**: none new. The plan file lives in the worktree (`.codypendent/plans/*.md`); the queued continuation lives in `pending_prompts` (0037); the parked question lives in `questions` (0034). **No migration — 0039 stays free.**
- **Policy**: `EvalContext.plan_write_root`/`write_target` are in-memory evaluation inputs, never serialized; `PathScope` (already `Serialize`) is reused unchanged.

## 7. Acceptance criteria

1. RUN `cargo test -p codypendent-daemon policy` — EXPECT: a Plan-mode `EvalContext` (`with_plan_write_root(<wt>/.codypendent/plans)` + `with_write_target(<wt>/.codypendent/plans/x.md)`) evaluates `WritePatch` to `Allow` with reason `policy.plan-file-write-allowed`; the same context with `write_target = <wt>/src/main.rs` evaluates to `Deny` with `policy.write-denied-by-mode`; a Plan context with **no** `write_target` denies (the `git.apply_patch` case).
2. A live Plan run refuses a write to a non-plan file: RUN a Plan-mode agent turn that calls `workspace.write_file` on `src/lib.rs` — EXPECT a `ToolDenied` + `policy denied: the active mode forbids filesystem writes` observation; a `workspace.write_file` on `.codypendent/plans/plan.md` in the same run SUCCEEDS and the file exists.
3. `offered_tool_names` for a Plan run INCLUDES `workspace.write_file` and `workspace.edit_file`, EXCLUDES `git.apply_patch`, `shell.run` stays offered (commands allowed), and `web.search`/GitHub tools are excluded (network denied).
4. `plan_exit` round-trips through the shipped question tool and the queue: RUN a Plan run that calls `plan_exit`; answer the parked question `Yes` — EXPECT a `PendingPromptsChanged` with one `Build`-mode entry, and when the Plan run completes, a `SubmitUserInput { mode: Build }` continuation launches whose text contains "has been approved, you can now edit files". Answering `No` enqueues nothing and the observation is "Staying in Plan mode".
5. `plan_exit` reject-with-feedback: dismiss the question with feedback "add error handling to the plan first" — EXPECT no enqueue and a `plan transition declined … add error handling` observation (adoption 03 path).
6. `plan_enter` is offered only in Build mode and `plan_exit` only in Plan mode: RUN `advertised_tool_definitions` for each mode — EXPECT the gating holds; both are absent when `plan_bridge`/`questions` is unwired (workflow node run).
7. `plan_enter` accepted enqueues a `Plan`-mode continuation carrying the request (arg or objective).
8. House rules: no new migration; `cargo clippy --workspace --all-targets -- -D warnings` clean; deny-wins untouched (the exception only adds a narrow allow); no `unsafe`.
9. RUN `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` — EXPECT green (plus the `doc-counts` + `extension` gates after adding the `ProposedAction::PlanTransition` round-trip line).

## 8. Tests

- `crates/daemon/src/policy/mod.rs`: `plan_mode_allows_a_write_under_the_plan_root`, `plan_mode_denies_a_write_outside_the_plan_root`, `plan_mode_denies_a_write_with_no_target` (apply_patch case), `plan_transition_is_always_allowed_and_not_reusable`, and a regression that a **Build** context (`plan_write_root == None`) is unaffected.
- `crates/protocol/src/run.rs`: extend the `ProposedAction` round-trip test with `PlanTransition`; assert a future-tag `ProposedAction` still deserializes to `Unknown`.
- `crates/runtime` (agent.rs in-file tests, following adoption 03's `AskUser` stubs — stub `QuestionChannel` + a recording `PlanBridge`):
  `plan_run_offers_plan_writers_not_apply_patch`,
  `plan_run_writes_the_plan_file_but_refuses_the_worktree` (drives `run_tool` for both targets and asserts the observations),
  `plan_exit_yes_enqueues_a_build_continuation` (asserts the `PlanBridge::switch_mode(Build, text)` call),
  `plan_exit_no_enqueues_nothing`,
  `plan_exit_reject_feedback_reaches_the_observation`,
  `plan_enter_is_build_only_and_plan_exit_is_plan_only` (advertisement gating),
  `plan_tools_absent_without_a_bridge`.
- `crates/runtime/src/tools/plan.rs`: name constants.
- Integration (daemon test harness with the real `QuestionBroker` + `prompt_queue`, extending adoption 06's drainer tests): `plan_exit_yes_drains_a_build_turn_when_the_plan_run_completes`, `plan_switch_is_idempotent_on_the_run_id` (a duplicated `switch_mode` enqueues once).

## 9. Gotchas

1. **The plan file must be writable while everything else is denied** — do NOT set `mode_overlay(Plan).write_allowed = true`; that would re-permit git-shell mutations (`eval_command` line 627) and every worktree write. The exception is additive inside the `!write_allowed` branch, gated on the resolved target being under `.codypendent/plans/`.
2. **`WritePatch` must stay pathless on the wire** — the target rides `EvalContext.write_target` (internal), never the action. Adding a path field to `WritePatch` would churn its golden vector and leak model paths into approval/audit records; the wire type is unchanged.
3. **Resolve the write target against the worktree before comparing** — a model that passes a relative `.codypendent/plans/plan.md` must be joined to the worktree, or `PathScope::allows` canonicalizes it against the daemon's cwd and the exception silently fails (denying a legitimate plan write). `prepared_write_target` does the join.
4. **`offered_tool_names`' invariant now holds *because* policy allows the plan write** — the module doc says the filter "only ever REMOVES a name the overlay's policy evaluation would deny." Re-offering `write_file`/`edit_file` in Plan mode is sound only alongside §5.1; ship them in the same change or a Plan run advertises a tool policy still denies.
5. **The synthetic message must be recorded in-band** — enqueue through the `QueuePrompt` command path (durable, ledgered `PendingPromptsChanged`, idempotent), never a private in-memory hop. A crash between "operator said yes" and "continuation launched" must replay to the same queued turn; the `plan-switch:<run_id>` idempotency key guarantees exactly one continuation.
6. **`plan_exit` yes/no comes back as an *answer*, not a rejection** — in the shipped question tool, selecting the "No" option returns `Answered(["No"])`; only a dismissed card returns `Rejected`. The handler must branch on the answer label ("Yes" → switch, "No" → stay) and treat `Rejected{feedback}` as the correction path. Do not equate "No" with reject.
7. **Both seams gate advertisement, not just one** — a run with a `QuestionChannel` but no `PlanBridge` (or vice versa) must not advertise the tools, or a park with no way to enqueue leaves the model stranded. Gate on `questions.is_some() && plan_bridge.is_some()`.
8. **Park then enqueue, in that order** — the confirmation question parks the *current* run in `WaitingForUserInput` (adoption 03); the enqueue happens only after the answer and only on "yes". The queued continuation drains when the *current* Plan run reaches `Completed` (adoption 06's drain rule), so the model must end its turn after `plan_exit` returns — the tool description and `PLAN_MODE_INSTRUCTION` both say so.
9. **`custom: false` is the internal caller adoption 03 reserved** — the tool schema still never advertises `custom`; the plan tools set it on the internal `QuestionPrompt` they build, so the yes/no is strict (no free-text row) exactly as opencode's `plan_exit`.
10. **Interactive-only wiring** — like the `QuestionChannel`, wire the `PlanBridge` only for interactive session runs in `executor.rs`; a workflow node that could enqueue a mode switch into a session it doesn't own is a cross-surface leak.

## 10. Out of scope

- A session-level persisted "current mode" — mode is per-turn (`SubmitUserInput.mode`); the switch is just the next queued turn's mode. No session mode column.
- Auto-entering Plan without the operator's yes (opencode also requires the confirmation).
- The plan file's *content* format/linting, slug/timestamp naming scheme, or a plan index — the model names the file under `.codypendent/plans/`; any `.md` there is writable.
- `git.apply_patch` in Plan mode (multi-file diffs can't be scoped to the plan directory; the plan is authored with `write_file`/`edit_file`).
- Extending the plan-file exception to other modes (Review/Ask stay strictly read-only).
- A dedicated build-switch system reminder beyond the in-band approval text (§5.6) — optional polish, not required for the round-trip.
- ACP / Remote UI surfaces for the bridge tools.
