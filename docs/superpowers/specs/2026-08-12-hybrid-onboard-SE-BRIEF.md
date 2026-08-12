# SE agent brief — Hybrid onboard + discovery trust (ship B)

Pass this package to the software-engineer agent. Do **not** improvise product scope.

## Read in order

1. **Design (contracts):** [`docs/superpowers/specs/2026-08-12-hybrid-onboard-discovery-design.md`](../specs/2026-08-12-hybrid-onboard-discovery-design.md)
2. **Plan (execute task-by-task):** [`docs/superpowers/plans/2026-08-12-hybrid-onboard-discovery.md`](../plans/2026-08-12-hybrid-onboard-discovery.md)
3. **Prior art:**
   - [`docs/superpowers/specs/2026-07-24-tui-add-model-design.md`](../specs/2026-07-24-tui-add-model-design.md)
   - [`docs/superpowers/specs/2026-07-26-model-discovery-design.md`](../specs/2026-07-26-model-discovery-design.md)
   - [`docs/superpowers/plans/2026-07-26-model-discovery.md`](../plans/2026-07-26-model-discovery.md) (pattern reference for Intent→harness→Action)

## Locked product

- Hybrid onboard (**C**): wizard only when **zero runnable** models
- Ship **B** only: onboard + discovery trust
- API key + env prefill; **no OAuth**; agents via **ACP**
- Architecture **A**: `Overlay::Onboard` = Triage + SkipConfirm; hand off to existing AddModel/ACP flows with `onboard_active`
- Competitive workbench UX: persistent session strip, responsive command/model/provider surfaces, larger composer, actionable failures, and first-class council/subagent activity
- Curated growth loop: bounded factual memory, reusable procedural skills, provenance/confidence, visible journey, edit/delete/pin, and repository/user/council scoping

## Must-fix bugs in same ship

1. `ProviderCard.has_key` / queries ignore env → use auth.json **∪** catalog env
2. `ProviderModelsLoaded` race → add `request_id`
3. Blank `AddModelKey` writes keyless model → reject

## Do not build

OAuth, an arbitrary shell-executed status-line plugin, silent learning from untrusted tool/web output, raw-transcript memory capture, or a second parallel preference store.

## Evidence (optional)

`.cursor-tmp/tui-audit-r3/06-env-key-*`, `07-provider-race-*`, `05-add-model-*`

## Execution

Use **subagent-driven-development** or **executing-plans** against the plan checkboxes. Commit per task as the plan says (unless the human says otherwise).
