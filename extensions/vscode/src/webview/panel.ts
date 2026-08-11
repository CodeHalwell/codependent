/**
 * Webview transcript panel — pure HTML/JS generation (no `vscode` import, so it
 * is importable in tests). `extension.ts` supplies a nonce and the webview's
 * `cspSource` and posts `TranscriptMessage`s in; the panel renders the session
 * transcript and current run state, and posts approval decisions back out.
 *
 * The panel keeps only VIEW state (a rolling transcript in the DOM). Session
 * truth lives in the daemon's ledger and is recovered on reload via
 * attach-resume, so nothing here is authoritative.
 */
import { randomBytes } from "node:crypto";
export type { TranscriptMessage, WebviewCommandMessage } from "./messages.js";

export interface PanelHtmlOptions {
  nonce: string;
  cspSource: string;
  scriptUri?: string;
}

/**
 * Build the full webview HTML. A strict CSP allows only the nonce'd inline
 * script and styles from `cspSource`; there are no external resources.
 */
export function renderPanelHtml(options: PanelHtmlOptions): string {
  const { nonce, cspSource, scriptUri } = options;
  const csp = [
    "default-src 'none'",
    `style-src ${cspSource} 'nonce-${nonce}'`,
    `script-src ${cspSource} 'nonce-${nonce}'`,
    `img-src ${cspSource} data: blob:`,
    `media-src ${cspSource} data: blob:`,
    "font-src data:",
  ].join("; ");

  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8" />
<meta http-equiv="Content-Security-Policy" content="${csp}" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>Codypendent Session</title>
<style nonce="${nonce}">
  :root { color-scheme: light dark; }
  body {
    font-family: var(--vscode-font-family, sans-serif);
    font-size: var(--vscode-font-size, 13px);
    color: var(--vscode-foreground);
    padding: 0.5rem;
    margin: 0;
  }
  header { display: flex; align-items: center; gap: 0.5rem; margin-bottom: 0.5rem; }
  .status { font-size: 11px; padding: 2px 6px; border-radius: 3px;
    background: var(--vscode-badge-background); color: var(--vscode-badge-foreground); }
  .run-state { font-size: 11px; opacity: 0.8; }
  #transcript { display: flex; flex-direction: column; gap: 4px; }
  .entry { padding: 4px 6px; border-left: 2px solid var(--vscode-panel-border);
    white-space: pre-wrap; word-break: break-word; }
  .entry .label { font-weight: 600; }
  .entry .seq { opacity: 0.5; font-size: 10px; margin-right: 6px; }
  .approval { border: 1px solid var(--vscode-inputValidation-warningBorder, #b89500);
    border-radius: 4px; padding: 6px; margin: 4px 0; }
  .approval .risk { font-size: 11px; opacity: 0.8; }
  .approval .actions { display: flex; gap: 6px; margin-top: 6px; }
  button {
    font-family: inherit; font-size: 12px; border: none; border-radius: 3px;
    padding: 3px 10px; cursor: pointer; touch-action: manipulation;
    color: var(--cody-ui-text-on-accent, var(--cody-ui-color-accentforeground, var(--vscode-button-foreground)));
    background: var(--cody-ui-action-primary, var(--cody-ui-color-accent, var(--vscode-button-background)));
  }
  button.reject { background: var(--vscode-button-secondaryBackground);
    color: var(--vscode-button-secondaryForeground); }
  button:hover { background: var(--vscode-button-hoverBackground); }
  button:focus-visible, a:focus-visible, summary:focus-visible, [tabindex]:focus-visible {
    outline: 2px solid var(--cody-ui-focus-active, var(--cody-ui-color-focus, var(--vscode-focusBorder))); outline-offset: 2px;
  }
  button:disabled { cursor: not-allowed; opacity: .55; }
  .resolved { opacity: 0.6; font-size: 11px; }
  #remote-ui { display: contents; }
  .remote-ui-root { display: flex; flex-direction: column; gap: var(--cody-ui-spacing-md, .5rem); margin-bottom: .75rem; min-width: 0;
    color: var(--cody-ui-text-primary, var(--cody-ui-color-foreground, var(--vscode-foreground)));
    background: var(--cody-ui-surface-background, var(--cody-ui-color-background, transparent)); }
  .ui-host-shell { display: grid; grid-template-columns: minmax(0, 1fr); grid-template-areas:
    "navigation" "sidebar" "primary" "transcript" "composer" "setup" "status"; gap: .625rem; min-width: 0; }
  .ui-host-region { display: grid; gap: .5rem; min-width: 0; }
  .ui-host-region-sidebar { grid-area: sidebar; }
  .ui-host-region-navigation { grid-area: navigation; }
  .ui-host-region-primary { grid-area: primary; }
  .ui-host-region-transcript { grid-area: transcript; }
  .ui-host-region-composer { grid-area: composer; position: sticky; bottom: 0; z-index: 4;
    background: var(--cody-ui-surface-background, var(--cody-ui-color-background, var(--vscode-sideBar-background))); }
  .ui-host-region-setup { grid-area: setup; }
  .ui-host-region-status { grid-area: status; position: sticky; bottom: 0; z-index: 3; }
  .ui-host-region-overlay { position: fixed; inset: 2.5rem .5rem .5rem; z-index: 100; pointer-events: none;
    display: grid; align-content: start; justify-items: stretch; gap: .5rem; }
  .ui-host-region-overlay > .ui-contribution-group { pointer-events: auto; max-height: min(70vh, 40rem); overflow: auto;
    overscroll-behavior: contain;
    grid-area: 1 / 1;
    border: 1px solid var(--cody-ui-surface-border, var(--cody-ui-color-border, var(--vscode-panel-border))); border-radius: 6px; padding: .625rem;
    background: var(--cody-ui-surface-overlay, var(--cody-ui-surface-background, var(--cody-ui-color-background, var(--vscode-editorWidget-background))));
    box-shadow: 0 8px 28px var(--vscode-widget-shadow); }
  .ui-host-region-overlay > .ui-contribution-group[inert] { visibility: hidden; pointer-events: none; }
  .ui-host-region-overlay > .ui-slot-notification { justify-self: end; align-self: start; width: min(24rem, calc(100vw - 1rem)); z-index: 3; }
  .ui-host-region-overlay > .ui-contribution-group:not([inert]):not(.ui-slot-notification) { z-index: 2; }
  .ui-contribution-group, .ui-document { display: flex; flex-direction: column; gap: .5rem; }
  .ui-contribution-group { min-width: 0; overflow-wrap: anywhere; }
  .ui-extension-chrome { display: flex; align-items: center; justify-content: space-between; gap: .75rem;
    padding: .25rem .5rem; border: 1px solid var(--vscode-panel-border); border-radius: 4px;
    color: var(--vscode-descriptionForeground); background: var(--vscode-sideBar-background); font-size: 11px; }
  .ui-extension-chrome strong { color: var(--vscode-foreground); font-family: var(--vscode-editor-font-family); }
  [data-ui-node-id] { min-width: 0; overflow-wrap: anywhere; }
  .ui-layout { box-sizing: border-box; min-width: 0; }
  .ui-bordered, .ui-domain-card, .ui-feedback { border: 1px solid var(--cody-ui-surface-border, var(--cody-ui-color-border, var(--vscode-panel-border))); border-radius: 4px; padding: var(--cody-ui-spacing-md, .5rem); }
  .ui-split { min-width: 0; min-height: 0; }
  .ui-muted { color: var(--vscode-descriptionForeground); }
  .ui-text { white-space: pre-wrap; overflow-wrap: anywhere; }
  .ui-markdown p { margin: .35rem 0; }
  .ui-markdown h1, .ui-markdown h2, .ui-markdown h3, .ui-markdown h4 { margin: .7rem 0 .35rem; }
  .ui-markdown-bullet { padding-inline-start: 1rem; }
  .ui-code, .ui-diff pre, .ui-terminal-preview pre { overflow: auto; margin: .25rem 0; padding: .5rem; background: var(--vscode-textCodeBlock-background); }
  .ui-diff span { display: block; min-height: 1em; }
  .diff-add { color: var(--vscode-gitDecoration-addedResourceForeground); background: color-mix(in srgb, var(--vscode-gitDecoration-addedResourceForeground) 12%, transparent); }
  .diff-remove { color: var(--vscode-gitDecoration-deletedResourceForeground); background: color-mix(in srgb, var(--vscode-gitDecoration-deletedResourceForeground) 12%, transparent); }
  .ui-image { display: block; max-width: 100%; max-height: 32rem; object-fit: contain; }
  .ui-media-fallback, .ui-unsupported { border: 1px dashed var(--vscode-panel-border); padding: .5rem; }
  .ui-list { margin: .25rem 0; padding-inline-start: 1.5rem; }
  .ui-table-scroll { max-width: 100%; overflow: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { border-bottom: 1px solid var(--vscode-panel-border); padding: .3rem .4rem; text-align: start; vertical-align: top; }
  .ui-key-value, .ui-json dl { display: grid; grid-template-columns: minmax(6rem, max-content) 1fr; gap: .2rem .5rem; }
  .ui-key-value dt, .ui-json dt { font-weight: 600; }
  .ui-key-value dd, .ui-json dd { margin: 0; min-width: 0; overflow-wrap: anywhere; }
  .ui-tree [role=group] { padding-inline-start: 1rem; border-inline-start: 1px solid var(--vscode-tree-indentGuidesStroke); }
  .ui-chart svg { width: 100%; height: 5rem; color: var(--vscode-charts-blue); stroke: currentColor; }
  .ui-badge { display: inline-flex; border-radius: 999px; padding: .1rem .45rem; background: var(--vscode-badge-background); color: var(--vscode-badge-foreground); }
  .ui-progress { display: grid; gap: .2rem; }
  .ui-progress progress { width: 100%; }
  .ui-spinner { display: inline-block; animation: ui-spin 1s linear infinite; }
  .ui-navigation, .ui-tabs [role=tablist], .ui-pagination, .ui-domain-card > header { display: flex; flex-wrap: wrap; align-items: center; gap: .35rem; }
  .ui-tabs [aria-selected=true] { outline: 1px solid var(--cody-ui-color-focus, var(--vscode-focusBorder)); }
  .ui-form { display: flex; flex-direction: column; gap: .5rem; }
  .ui-field { display: grid; gap: .2rem; }
  .ui-field input, .ui-field textarea, .ui-field select { box-sizing: border-box; width: 100%; color: var(--vscode-input-foreground); background: var(--vscode-input-background); border: 1px solid var(--vscode-input-border, transparent); padding: .35rem .45rem; font: inherit; }
  .ui-field input:focus, .ui-field textarea:focus, .ui-field select:focus { outline: 1px solid var(--vscode-focusBorder); }
  .ui-choice { display: flex; gap: .4rem; align-items: center; }
  .ui-link-button { color: var(--vscode-textLink-foreground); background: none; padding: 0; }
  .ui-domain-card > header { justify-content: space-between; }
  .ui-host-errors { display: grid; gap: .5rem; }
  .ui-host-error { display: grid; gap: .35rem; min-width: 0; border: 1px solid var(--vscode-inputValidation-errorBorder); padding: .5rem; overflow-wrap: anywhere; }
  .ui-host-error code { overflow-wrap: anywhere; white-space: pre-wrap; }
  .ui-host-error-message { white-space: pre-wrap; }
  .ui-host-error-actions { display: flex; flex-wrap: wrap; gap: .35rem; }
  .ui-secondary-button { background: var(--vscode-button-secondaryBackground); color: var(--vscode-button-secondaryForeground); }
  .tone-positive { color: var(--cody-ui-status-success, var(--cody-ui-color-positive, var(--vscode-testing-iconPassed))); }
  .tone-warning { color: var(--cody-ui-status-warning, var(--cody-ui-color-warning, var(--vscode-editorWarning-foreground))); }
  .tone-critical { color: var(--cody-ui-status-error, var(--cody-ui-color-critical, var(--vscode-errorForeground))); }
  .tone-info { color: var(--cody-ui-status-info, var(--cody-ui-color-info, var(--vscode-textLink-foreground))); }
  @keyframes ui-spin { to { transform: rotate(360deg); } }
  @media (min-width: 48rem) {
    .ui-host-shell { grid-template-columns: minmax(12rem, .32fr) minmax(0, 1fr); grid-template-areas:
      "navigation navigation" "sidebar primary" "sidebar transcript" "composer composer" "setup setup" "status status"; }
  }
  @media (prefers-reduced-motion: reduce) { *, *::before, *::after { scroll-behavior: auto !important; transition-duration: .01ms !important; animation-duration: .01ms !important; animation-iteration-count: 1 !important; } .ui-spinner { animation: none; } }
  @media (forced-colors: active) { .ui-bordered, .ui-domain-card, .ui-feedback { border-color: CanvasText; } }
</style>
</head>
<body>
<header>
  <span class="status" id="status" role="status" aria-live="polite">closed</span>
  <span class="run-state" id="run-state" role="status" aria-live="polite"></span>
</header>
<div id="approvals"></div>
<div id="remote-ui"></div>
<div id="transcript"></div>
<script nonce="${nonce}">
  const vscode = acquireVsCodeApi();
  const statusEl = document.getElementById('status');
  const runStateEl = document.getElementById('run-state');
  const transcriptEl = document.getElementById('transcript');
  const approvalsEl = document.getElementById('approvals');
  const approvalNodes = new Map();

  const MAX_ENTRIES = 500;
  const scroller = document.scrollingElement || document.documentElement;
  function nearBottom() {
    return (scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight) < 40;
  }

  function addEntry(sequence, label, detail) {
    const stick = nearBottom();
    const entry = document.createElement('div');
    entry.className = 'entry';
    const seq = document.createElement('span');
    seq.className = 'seq';
    seq.textContent = sequence != null ? ('#' + sequence) : '';
    const lab = document.createElement('span');
    lab.className = 'label';
    lab.textContent = label + ' ';
    const det = document.createElement('span');
    det.textContent = detail || '';
    entry.appendChild(seq);
    entry.appendChild(lab);
    entry.appendChild(det);
    transcriptEl.appendChild(entry);
    // Cap the DOM so an hours-long streaming session does not grow unbounded.
    while (transcriptEl.childElementCount > MAX_ENTRIES) {
      transcriptEl.removeChild(transcriptEl.firstChild);
    }
    // Only autoscroll if the user was already at the bottom — don't yank the
    // view down while they are scrolled up reading history.
    if (stick) {
      entry.scrollIntoView({ block: 'end' });
    }
  }

  function addApproval(approvalId, summary, risk) {
    if (approvalNodes.has(approvalId)) return;
    const card = document.createElement('div');
    card.className = 'approval';
    card.setAttribute('role', 'region');
    card.setAttribute('aria-label', 'Approval request');
    const title = document.createElement('div');
    title.textContent = summary;
    const riskEl = document.createElement('div');
    riskEl.className = 'risk';
    riskEl.textContent = 'risk: ' + risk;
    const actions = document.createElement('div');
    actions.className = 'actions';
    const approve = document.createElement('button');
    approve.type = 'button';
    approve.textContent = 'Approve';
    approve.onclick = () => vscode.postMessage({ kind: 'approve', approvalId });
    const reject = document.createElement('button');
    reject.type = 'button';
    reject.className = 'reject';
    reject.textContent = 'Reject';
    reject.onclick = () => vscode.postMessage({ kind: 'reject', approvalId });
    actions.appendChild(approve);
    actions.appendChild(reject);
    card.appendChild(title);
    card.appendChild(riskEl);
    card.appendChild(actions);
    approvalsEl.appendChild(card);
    approvalNodes.set(approvalId, card);
  }

  function resolveApproval(approvalId, decision) {
    const card = approvalNodes.get(approvalId);
    if (!card) return;
    card.innerHTML = '';
    card.className = 'resolved';
    card.textContent = 'Approval ' + approvalId.slice(0, 8) + ' -> ' + decision;
  }

  window.addEventListener('message', (event) => {
    const msg = event.data;
    switch (msg.kind) {
      case 'status': statusEl.textContent = msg.status; break;
      case 'runState': runStateEl.textContent = 'run ' + msg.runId.slice(0, 8) + ': ' + msg.state; break;
      case 'event': addEntry(msg.sequence, msg.label, msg.detail); break;
      case 'approval': addApproval(msg.approvalId, msg.summary, msg.risk); break;
      case 'approvalResolved': resolveApproval(msg.approvalId, msg.decision); break;
      case 'clear':
        transcriptEl.innerHTML = '';
        approvalsEl.innerHTML = '';
        approvalNodes.clear();
        break;
    }
  });
</script>
${scriptUri === undefined ? "" : `<script nonce="${nonce}" src="${scriptUri}"></script>`}
</body>
</html>`;
}

/** A cryptographically strong nonce for the webview CSP. */
export function makeNonce(): string {
  return randomBytes(16).toString("hex");
}
