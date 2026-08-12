# Agent report: TUI experience

## Screen inventory (each: 1-line polish verdict)

| Surface (render.rs unless noted) | Verdict |
|---|---|
| Splash/welcome (`render_splash` :392) | Polished: wordmark, spinner, warnings cap, width degradation, Enter gate |
| Chat header (:272) | Good: priority-based field dropping; but mode chip shows `default_mode` not the run's (:318) |
| Conversation (:1201) | Good bones (centered 118-col measure, virtualized window, empty-state heroes :1217-1269); tool/patch expansion is dead (see Bugs), no timestamps |
| Composer (:1345) | Multi-line growth + tail-scroll is nice; end-of-string editing only — no Left/Right/Home/End cursor (input.rs:392-416, reduce.rs:302-316) |
| Status/footer line (:1897) | Polished: contextual states, pair-packing, clickable rows |
| Workspace 3-pane (:582) | OK; single-pane fallback <110 cols; panes read-only, focus only reachable by click (Tab is dead in Composer mode) |
| Approval modal (:4986) | Good: verbatim action/env/risk; input-owning modal |
| Help (:5052) | Fine, but shows bindings that are false in the base view (see Bugs) |
| Issues/diagnostics (:2295) | Polished: guidance per issue, clear-all, empty "all clear" |
| Command palette (:4809) | Polished: groups, ranked filter, key-hint column, click rows |
| Model picker (:2803) | Best-in-class here: readiness/current badges, detail rail, responsive `picker_regions` (:6233) |
| Provider picker (:3043) | Good: availability glyphs, catalog-only messaging |
| Mode picker (:3235), /keys (:3349) | Good; consistent modal_surface/shadow language |
| Add-model flow (5 overlays, :2228-2283, :5194, :5234) | Good flow logic; "Fetching…" box has no spinner |
| Council builder 7-step (:5519-6081) | Strong wizard: progress rail, member summary, validation, error-preserving retry |
| Skills (:2605) / Memory (:3540) | Decent list+detail; hardcoded footer hit offsets; no filter box |
| Docs Studio (:3736) | Most complex; 3-rail focus + lease indicator; cramped review rows; hardcoded chip offsets (:4007-4036) |
| Edges (:4043) + loading/empty (:4243/:4273) | Polished empty/loading states; DB-paged search |
| Workflow (:4348) / Blackboard (:4579) | Solid list+detail; no DAG drawing (see Gaps) |
| Remote UI surfaces (:98-266; remote_ui/paint.rs) | Serious engineering: 60+ primitives, sanitization, capability fallback, a11y projection |
| Accessible cooked mode (accessible.rs; cli/tui.rs:393) | Exemplary: full parity commands, ANSI/bidi stripping (:539-601) |

## Verified working (file:line)

- **Theme system**: 7 variants with WCAG-AA contrast tests (theme.rs:807-834), invisibility/semantic-pair invariants (:840-907), depth detection incl. NO_COLOR (:666-694); data-only theme packs structurally reject capabilities (theme_pack.rs:79-104); `--theme`/`CODYPENDENT_THEME` wiring with pack fallback (theme_select.rs:41-129). All widgets use tokens; `style_for` is exhaustive over `SpanRole` (render.rs:950-987).
- **Markdown**: tables with alignment + header rule (markdown.rs:380-459), per-language syntax highlight via synoptic (:468-511), nested/ordered lists (:240-255), blockquotes, links with trailing URL (:302-308); parse-once cache on finalize, streaming tail stays plain (reduce.rs:38-58; render.rs:1060-1081); 64KiB rich cap (reduce.rs:32).
- **State/reduce correctness**: universal `step`/`clamp` guards for empty lists (reduce.rs:3408-3428); transcript cap with index shifting (state.rs:1685-1692); per-entry byte cap (:1654-1678); zero-match filter guards on every picker submit (reduce.rs:2799-2816); race-guarded async model-list results (:3190-3252); selection preserved across projection refreshes (cli/tui.rs:1622-1700).
- **Scroll/follow contract**: renderer-cached `transcript_max_scroll` Cell drives PgUp leave/PgDn re-enter follow (state.rs:1305; reduce.rs:1680-1712; render.rs:1277-1290).
- **Secret hygiene**: `SecretKey` redacting Debug (action.rs:355-362), masked prompts (render.rs:5150-5188), tests (state.rs:1734-1788, action.rs:609-661); keys never on the wire (cli/tui.rs:1484-1490).
- **Mouse**: frame-fresh hit map with z-order resolution (render.rs:41, input.rs:210-216), modal shields (:6211), scrim-click dismiss (:2072-2074), scrolled-list row hits offset correctly (e.g. :2908-2913).
- **Resilience**: gap-repair/reattach with deadline (cli/tui.rs:928-1064, 1283-1298); crash log hook (:206); splash stage channel + warning drain (:271-341); unicode-safe truncate/tail in pickers (render.rs:6258-6304) and Remote UI text (remote_ui/text.rs:70-148).
- **Council**: full wizard → validated atomic `councils.toml` write via CLI host (reduce.rs:2625-2785; cli/tui.rs:1495-1534).
- **Test depth**: ~4,800 lines of reducer tests (reduce.rs:3451-8229) and ~3,800 of render tests (render.rs:6434-10260) — unusually strong.

## Bugs & broken wiring (file:line, why, severity)

1. **Tool cards and patch diffs cannot be expanded by any input — dead feature.** `fold_hit_entry` registers click targets only for Backstage, long Notes, and failed-run summaries (render.rs:1121-1133); `Tool`/`Patch` return `None`, so no click target. Keyboard: with `Overlay::None` the input mode is always `Composer` (state.rs:1457-1465), where Up/Down = history and Enter = send (input.rs:392-416) — `Action::Expand` fires only from `Normal` mode (browser overlays). `expand_selected` also requires `focus == Pane::Transcript` (reduce.rs:1728-1731), unreachable since Tab is unmapped in Composer mode. Net effect: expanded tool detail (args digest, error, artifact, render.rs:1701-1731) and the entire **diff preview renderer** (render.rs:1761-1799) are unreachable UI. **HIGH** (directly guts rubric 7).
2. **Wrap accounting mismatch → clipped tail / misplaced window.** Measurement assumes cell-wrap `ceil(cols/width)` (render.rs:833-841, 912-914) but drawing uses ratatui word-wrap (`Wrap{trim:false}` + `.scroll`, :1336-1339). Word wrap can produce more rows than the ceil estimate, so `max_scroll` underestimates: in follow mode the newest line(s) can sit below the viewport, and window building (:1145-1196) can misalign by a few rows on wrap-heavy transcripts. **MEDIUM**.
3. **Keyboard-parity claims are false in the base view.** `KEY_BINDINGS` advertises "↑↓ + Enter — activate a list row / transcript fold (same as clicking it)" and "Tab — focus a pane" (input.rs:103-112); neither works when no overlay is open (Composer mode). Help renders these misleading rows (render.rs:5064). The user guide's mouse column invents nonexistent controls ("Click Pause", "Click Skills Tab", "Click Layout Button" — docs/cli-and-tui-user-guide.md:94-121); no such buttons exist in render.rs. Violates the crate's own RULE 3. **MEDIUM**.
4. **`FOOTER_HINTS` is dead code**: the curated clickable footer strip with per-chip actions (input.rs:163-205) is never rendered; `render_status_line` builds its own spans and registers only whole-line hits (render.rs:1935, 1994, 2015). Drift-guard tests protect a feature that doesn't exist. **LOW**.
5. **Char-count truncation breaks alignment for wide glyphs.** `truncate` counts chars (render.rs:6413-6420) and is used across runs pane (:670), skills (:2666), memory (:3607), docs (:3792), edges (:4122-4131), workflow (:4405-4429), blackboard (:4635-4668); CJK/emoji overflow their columns. Markdown table layout likewise uses `chars().count()` for widths and padding (markdown.rs:386-414) → misaligned tables with wide characters. The correct `truncate_display_width` exists (:6258) but only the pickers use it. **MEDIUM**.
6. **Hard-coded footer-chip hit offsets** must match label strings by hand and are already slightly off (e.g. skills "M memory" chip at x+14/w8 over `"  ↑/↓ skill · M memory"`, render.rs:2766-2777; memory :3705-3728; docs :4007-4036; edges :4210-4240; workflow :4528-4558). Fragile; any label edit silently breaks click targets. **LOW-MED**.
7. **Blank `/keys` submit closes the prompt** instead of reopening it (reduce.rs:2888-2898), inconsistent with `AddModelId`, which reopens on blank (:2961-2969). **LOW**.
8. **Animations starve**: interactive redraw on ticks only every 25 ticks ≈5s (cli/tui.rs:1350-1353; `wants_periodic_draw`=false :496-498), so the edge-loading braille spinner (render.rs:4244-4247) barely animates and "Fetching models…" (:5194) has no spinner at all; `RunActivity` "working…" is static text (:1441-1448). **LOW-MED polish**.
9. **Vestigial drift**: `pane_at` claims the renderer's "30/40/30 split" but render_workspace uses 26/48/26 (input.rs:496-507 vs render.rs:601-607); it's production-dead (lib export + tests only). `nav`'s transcript arm conflates entry index with row scroll (reduce.rs:1670-1676). **LOW**.
10. **Wheel = 10-row page jump**: wheel maps to `ScrollPageUp/Down` (input.rs:472-479; PAGE=10, reduce.rs:1692) — jarring vs. the conventional 1-3 lines per notch. **LOW**.

## Gaps vs target outcomes

- **#1 Beautiful TUI**: foundation is genuinely strong (token discipline, modal language, empty states everywhere), but: no composer cursor movement/mid-line editing; no transcript timestamps; knowledge browsers unfilterable; no runtime theme switching (theme is a boot-time flag only — no `/theme` palette command; palette.rs:87-251 has none).
- **#2 ACP**: TUI can connect an ACP agent from `/provider` (reduce.rs:3114-3125 → cli/tui.rs:2610 `connect_acp_agent`, off-thread with refresh :1321-1345). Missing: *automatic model discovery from ACP agents* — connect writes exactly one `acp/<id>` profile; there is no surface enumerating the agent's own models/modes.
- **#3 Model selection**: largely delivered (picker + provider catalog + live `/models` discovery + `/keys`). Friction: all palette-only (no single-key), pinned `pending_model` invisible in the header (only the serving model of a live run shows, render.rs:280).
- **#4 Skill-writer & doc-writer**: no authoring surface. Skills is explicitly "read only" (render.rs:2613; palette.rs:202-205); Docs Studio's empty state says "Creation is not wired here" (render.rs:3778); block "editing" is insert-at-position-0 only (reduce.rs:2573-2589).
- **#5 DAG viewer**: Workflow view is a flat topological *list*; dependencies are a comma-joined string field (state.rs:794; render.rs:4500) — no graph/lane drawing; same for the code-graph inspector (list of edges). Agent-side access is out of this crate, but the TUI has no DAG visualization at all.
- **#6 AI council**: creation wizard is complete and polished, but the TUI cannot **list, inspect, run, or delete** councils — only `codypendent council …` CLI can (docs/cli-and-tui-user-guide.md:350-366).
- **#7 Rich chat**: markdown/tables/highlighting are real, but "rich" is undercut by: dead diff/tool expansion (Bug 1), plain streaming text, no timestamps, no role avatars beyond `⏺ codypendent`, raw `(url)` suffixes instead of OSC-8 hyperlinks (markdown.rs:302-308), no image/media rendering in the transcript (Remote-UI-only).
- **#8 Voice**: zero surface. Protocol supports audio input envelopes (crates/protocol/src/input.rs:1-31) but the composer emits plain-text intents only (reduce.rs:2903-2943) and the terminal offer hardcodes `audio_capture: false` (remote_ui_host.rs:311). No TTS anywhere.
- **#9 Top-k tool selection**: runtime concern; no TUI surface implicated (Skill browser lists everything, which is fine for inspection).
- **#10 Blackboard + kanban**: blackboard viewer exists (read-only, live, render.rs:4579). **Kanban: nothing** (workspace-wide grep finds no kanban/backlog surface); no natural-language backlog UI.

## Prioritized opportunities

1. **Un-dead tool/patch expansion** — add `Tool`/`Patch` arms to `fold_hit_entry` (render.rs:1121) and add a transcript-focus keyboard path (e.g. Alt-↑/↓ moves `transcript_selected`, Enter toggles) in `map_composer_key` + reduce. **S-M / very high** — unlocks the already-written diff renderer.
2. **Composer cursor editing** — cursor index in `AppState`, Left/Right/Home/End/Ctrl-W/Ctrl-U in `map_composer_key`, splice edits in `edit_prompt`. **M / high**.
3. **Fix wrap accounting** — either wrap rows manually at cell granularity before Paragraph (drop word-wrap) or measure with ratatui's `WordWrapper`; keeps follow-mode pinned to the true bottom. render.rs:833/1277. **M / high**.
4. **Display-width truncation everywhere** — replace `truncate` call sites with `truncate_display_width`; use `unicode-width` in `layout_table` (markdown.rs:387). **S / medium**.
5. **ASCII DAG lanes for Workflow** — topological columns + box-drawing connectors from `depends_on` (already structured pre-render in cli/tui.rs `load_workflows`); reuse `node_state_color`. **M-L / high (rubric 5)**.
6. **Council list/run in TUI** — `/council` becomes a browser (list → detail → "run with objective" prompt emitting a client-only intent to the CLI council runner). **M / high (rubric 6)**.
7. **`/theme` palette command with live preview** — theme is already a pure value threaded into `render`; store choice beside `SessionStore`. **S-M / medium**.
8. **Timestamps + turn metadata** — `SessionEvent.occurred_at` is already on the wire (dropped at fold, reduce.rs:1013); render dim right-aligned times per turn header. **S / medium**.
9. **Redraw-on-animation gating** — `redraw |= state has active spinner` (querying/loading/thinking) instead of the 25-tick gate (cli/tui.rs:1350); add spinner to `AddModelQuerying` and "working…". **S / medium**.
10. **Voice slice** — mic toggle in composer → harness records → STT → `InputEnvelope` audio block (protocol ready); TTS on finalized model turns. **L / rubric 8**.
11. **Kanban overlay** — group blackboard/backlog items into status columns; reuse blackboard subscription plumbing. **M-L / rubric 10**.
12. **Render or delete `FOOTER_HINTS`**; fix `KEY_BINDINGS`/help/user-guide drift. **S / medium (trust)**.

## Extra ideas

- Replace hardcoded chip hit offsets with a tiny "hint bar" builder that lays out labeled chips and registers hits from measured spans — kills bug class 6.
- OSC-8 hyperlinks + OSC-52 "copy last reply / copy code block" actions.
- Transcript search (`/find`) reusing the palette input shape; jump-to-match sets `scroll`.
- Scrollbar ghost on the conversation (offset/max already computed each frame).
- Per-run tabs strip in chat mode (Ctrl-↑/↓ is invisible today; a one-line run strip would make multi-run state legible).
- Provider picker: badge rows that `can_list_models` ("live list ✓") so users know Enter fetches vs. free-text.
- Streaming markdown: parse-on-newline for completed lines during streaming (cheap incremental upgrade from plain tail).
- Persist `LayoutMode`, theme, and composer history to disk (all currently session-local).
