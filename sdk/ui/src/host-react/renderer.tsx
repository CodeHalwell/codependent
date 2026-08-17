/** @jsxImportSource react */
import React, {
  Component,
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
  type FormEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { toAccessibleText } from "../accessibility.js";
import { projectDocument, supportsRequirement } from "../capabilities.js";
import {
  MINIMAL_TERMINAL_CAPABILITIES,
  UI_PROTOCOL_VERSION,
  type UiCapabilities,
  type UiElementNode,
  type UiEvent,
  type UiEventModifiers,
  type UiJsonValue,
  type UiNode,
  type UiProps,
} from "../protocol.js";

import type { MountedRemoteUi, RemoteUiStore } from "./store.js";
import { webSlotDefinition, type WebHostRegion, type WebSlotDefinition } from "./slot-registry.js";

export type RemoteUiRecoveryAction = "retry" | "disable" | "report";
export interface RemoteUiRecoveryRequest {
  action: RemoteUiRecoveryAction;
  documentId: string;
  extensionId?: string;
  message: string;
}

export interface RemoteUiRendererProps {
  store: RemoteUiStore;
  capabilities: UiCapabilities;
  dispatch: (event: UiEvent) => void;
  recover?: (request: RemoteUiRecoveryRequest) => void;
  showTerminalFallback?: boolean;
}

interface RendererActions {
  dispatch: (target: UiElementNode, type: UiEvent["type"], payload?: UiJsonValue, modifiers?: UiEventModifiers) => void;
}

interface RendererMeta {
  activeNodeId: React.MutableRefObject<string | undefined>;
  capabilities: UiCapabilities;
}

const RendererActionsContext = createContext<RendererActions | null>(null);
const RendererMetaContext = createContext<RendererMeta | null>(null);

export function RecoveryCard({ request, recover }: {
  request: Omit<RemoteUiRecoveryRequest, "action">;
  recover?: (request: RemoteUiRecoveryRequest) => void;
}): ReactNode {
  const submit = (action: RemoteUiRecoveryAction): void => recover?.({ ...request, action });
  return (
    <section className="ui-host-error" role="alert" aria-live="polite" data-ui-error-document={request.documentId}>
      <strong>Extension surface unavailable</strong>
      <span className="ui-host-error-message">{request.message}</span>
      <code>{request.documentId}</code>
      <div className="ui-host-error-actions" role="group" aria-label="Extension surface recovery">
        <button type="button" onClick={() => submit("retry")}>Retry</button>
        <button type="button" className="reject" disabled={request.extensionId === undefined} onClick={() => submit("disable")}>Disable extension surface</button>
        <button type="button" className="ui-secondary-button" onClick={() => submit("report")}>Report details</button>
      </div>
    </section>
  );
}

interface RemoteDocumentBoundaryProps {
  documentId: string;
  revision: number;
  extensionId?: string;
  recover?: (request: RemoteUiRecoveryRequest) => void;
  children: ReactNode;
}

interface RemoteDocumentBoundaryState {
  message?: string | undefined;
  revision: number;
}

class RemoteDocumentBoundary extends Component<RemoteDocumentBoundaryProps, RemoteDocumentBoundaryState> {
  override state: RemoteDocumentBoundaryState = { revision: this.props.revision };

  static getDerivedStateFromError(error: unknown): Partial<RemoteDocumentBoundaryState> {
    return { message: error instanceof Error ? error.message : String(error) };
  }

  static getDerivedStateFromProps(props: RemoteDocumentBoundaryProps, state: RemoteDocumentBoundaryState): Partial<RemoteDocumentBoundaryState> | null {
    return props.revision === state.revision ? null : { revision: props.revision, message: undefined };
  }

  override render(): ReactNode {
    if (this.state.message === undefined) return this.props.children;
    return <RecoveryCard request={{
      documentId: this.props.documentId,
      ...(this.props.extensionId === undefined ? {} : { extensionId: this.props.extensionId }),
      message: this.state.message,
    }} recover={(request) => {
      if (request.action === "retry") this.setState({ message: undefined, revision: this.props.revision });
      this.props.recover?.(request);
    }} />;
  }
}

function useRendererActions(): RendererActions {
  const value = useContext(RendererActionsContext);
  if (value === null) throw new Error("Remote UI primitive rendered outside its document provider");
  return value;
}

function useRendererMeta(): RendererMeta {
  const value = useContext(RendererMetaContext);
  if (value === null) throw new Error("Remote UI primitive rendered outside its document provider");
  return value;
}

const SIZES: Readonly<Record<string, string>> = {
  xs: "0.125rem", sm: "0.25rem", md: "0.5rem", lg: "0.75rem", xl: "1rem",
};
const SAFE_DIMENSION = /^(?:auto|0|[0-9]+(?:\.[0-9]+)?(?:px|%|rem|em|ch|vh|vw))$/;
const SAFE_LINK_PROTOCOLS = new Set(["https:", "http:", "mailto:"]);
const MAX_STATIC_ITEMS = 5_000;
const MAX_JSON_ENTRIES = 1_000;

function stringProp(props: UiProps, key: string, fallback = ""): string {
  return typeof props[key] === "string" ? props[key] : fallback;
}

function numberProp(props: UiProps, key: string, fallback = 0): number {
  const value = props[key];
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function booleanProp(props: UiProps, key: string, fallback = false): boolean {
  return typeof props[key] === "boolean" ? props[key] : fallback;
}

function objectValue(value: UiJsonValue | undefined): Readonly<Record<string, UiJsonValue>> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Readonly<Record<string, UiJsonValue>>
    : undefined;
}

function arrayValue(value: UiJsonValue | undefined): readonly UiJsonValue[] {
  return Array.isArray(value) ? value : [];
}

function displayValue(value: UiJsonValue | undefined): string {
  if (value === undefined) return "";
  if (typeof value === "string") return value;
  if (value === null) return "null";
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function spacing(value: UiJsonValue | undefined): string | undefined {
  if (typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 256) return `${value}px`;
  if (typeof value === "string") return SIZES[value];
  return undefined;
}

function dimension(value: UiJsonValue | undefined): string | undefined {
  if (typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 100_000) return `${value}px`;
  return typeof value === "string" && SAFE_DIMENSION.test(value) ? value : undefined;
}

/**
 * `Grid.columns` is a track count or an explicit track list — the same
 * contract the terminal host normalizes (`crates/tui/src/remote_ui/codec.rs`).
 * Only the safe subset of track syntax survives into the CSP-restricted
 * host: `Nfr`, a safe dimension, `auto`, or a plain number of pixels.
 */
function gridTemplateColumns(value: UiJsonValue | undefined): string {
  if (Array.isArray(value)) {
    const tracks = value.slice(0, 24).flatMap((track) => {
      if (typeof track === "number" && Number.isFinite(track) && track >= 0) return [`${track}px`];
      if (typeof track !== "string") return [];
      if (/^[0-9]+(?:\.[0-9]+)?fr$/.test(track)) return [track];
      const safe = dimension(track);
      return safe === undefined ? [] : [`minmax(0, ${safe})`];
    });
    if (tracks.length > 0) return tracks.join(" ");
  }
  const count = typeof value === "number" && Number.isFinite(value) ? Math.trunc(value) : 2;
  return `repeat(${Math.max(1, Math.min(24, count))}, minmax(0, 1fr))`;
}

function align(value: UiJsonValue | undefined): CSSProperties["alignItems"] {
  switch (value) {
    case "start": return "flex-start";
    case "center": return "center";
    case "end": return "flex-end";
    case "stretch": return "stretch";
    case "spaceBetween": return "space-between";
    case "spaceAround": return "space-around";
    default: return undefined;
  }
}

function layoutStyle(props: UiProps, direction?: "row" | "column"): CSSProperties {
  return {
    ...(direction === undefined ? {} : { display: "flex", flexDirection: direction }),
    gap: spacing(props.gap),
    padding: spacing(props.padding),
    alignItems: align(props.align),
    width: dimension(props.width),
    height: dimension(props.height),
    flexGrow: typeof props.grow === "number" ? props.grow : undefined,
    flexWrap: props.wrap === true ? "wrap" : undefined,
  };
}

function modifiers(event: unknown): UiEventModifiers {
  const value = event as { altKey?: boolean; ctrlKey?: boolean; metaKey?: boolean; shiftKey?: boolean };
  return { alt: value.altKey ?? false, control: value.ctrlKey ?? false, meta: value.metaKey ?? false, shift: value.shiftKey ?? false };
}

function formObject(form: HTMLFormElement | null): UiJsonValue {
  if (form === null) return {};
  const output: Record<string, UiJsonValue> = {};
  try {
    const formData = new FormData(form);
    formData.forEach((raw, name) => {
      if (typeof raw !== "string") return;
      const current = output[name];
      if (current === undefined) output[name] = raw;
      else if (Array.isArray(current)) output[name] = [...current, raw];
      else output[name] = [current, raw];
    });
  } catch {
    // Fallback for form parsing errors
  }
  return output;
}

function safeMediaSource(value: string, kind: "image" | "audio"): string | undefined {
  if (typeof value !== "string" || value.length > 2_000_000) return undefined;
  if (value.startsWith("vscode-webview-resource:") || value.startsWith("blob:") || value.startsWith("tauri:") || value.startsWith("asset:")) return value;
  if (kind === "image" && /^data:image\/(?:png|jpeg|gif|webp);base64,/i.test(value)) return value;
  if (kind === "audio" && /^data:audio\/(?:mpeg|ogg|wav|webm);base64,/i.test(value)) return value;
  return undefined;
}

function safeLink(value: string): string | undefined {
  try {
    const parsed = new URL(value);
    return SAFE_LINK_PROTOCOLS.has(parsed.protocol) ? parsed.toString() : undefined;
  } catch {
    return undefined;
  }
}

function nodeLabel(node: UiElementNode): string {
  return stringProp(node.props, "accessibleLabel")
    || stringProp(node.props, "label")
    || stringProp(node.props, "title")
    || stringProp(node.props, "alt")
    || node.type;
}

function eventPayload(_node: UiElementNode, extra: Record<string, UiJsonValue> = {}): UiJsonValue | undefined {
  return Object.keys(extra).length === 0 ? undefined : extra;
}

function activationEvent(node: UiElementNode): "action" | "press" {
  return arrayValue(node.props.eventHandlers).includes("press") ? "press" : "action";
}

function NodeChildren({ node }: { node: UiElementNode }): ReactNode {
  return (node.children ?? []).map((child, index) => <RemoteNode key={child.id ?? `${node.id ?? node.type}-${index}`} node={child} />);
}

function GenericItems({ items, ordered = false }: { items: readonly UiJsonValue[]; ordered?: boolean }): ReactNode {
  const Element = ordered ? "ol" : "ul";
  return (
    <Element className="ui-list">
      {items.slice(0, MAX_STATIC_ITEMS).map((item, index) => {
        const record = objectValue(item);
        const label = record === undefined ? displayValue(item) : displayValue(record.label ?? record.title ?? record.value ?? item);
        const key = record === undefined ? `${index}-${label}` : String(record.id ?? record.key ?? `${index}-${label}`);
        return <li key={key}>{label}</li>;
      })}
      {items.length > MAX_STATIC_ITEMS ? <li className="ui-muted">{items.length - MAX_STATIC_ITEMS} more items omitted; use VirtualList to inspect the full collection.</li> : null}
    </Element>
  );
}

function LayoutPrimitive({ node }: { node: UiElementNode }): ReactNode {
  const props = node.props;
  const title = stringProp(props, "title");
  const border = props.border === true || typeof props.border === "string";
  const direction = node.type === "Row" ? "row" : node.type === "Stack" ? "column" : undefined;
  if (node.type === "Spacer") return <span aria-hidden="true" style={{ display: "block", minWidth: spacing(props.size), minHeight: spacing(props.size), flex: 1 }} />;
  if (node.type === "Split") {
    const horizontal = props.direction !== "vertical";
    const ratio = Math.min(0.9, Math.max(0.1, numberProp(props, "ratio", 0.5)));
    return (
      <div className="ui-split" style={layoutStyle(props, horizontal ? "row" : "column")}>
        {node.children.map((child, index) => (
          <div key={child.id ?? index} style={{ flex: index === 0 ? ratio : 1 - ratio, minWidth: 0, minHeight: 0 }}>
            <RemoteNode node={child} />
          </div>
        ))}
      </div>
    );
  }
  const style = layoutStyle(props, direction);
  if (node.type === "Grid") {
    style.display = "grid";
    style.gridTemplateColumns = gridTemplateColumns(props.columns);
  }
  if (node.type === "ScrollArea") {
    style.overflowX = props.axis === "vertical" ? "hidden" : "auto";
    style.overflowY = props.axis === "horizontal" ? "hidden" : "auto";
  }
  const body = <NodeChildren node={node} />;
  return title.length > 0
    ? <fieldset className={`ui-layout ${border ? "ui-bordered" : ""}`} style={style}><legend>{title}</legend>{body}</fieldset>
    : <div className={`ui-layout ${border ? "ui-bordered" : ""}`} style={style} aria-label={stringProp(props, "accessibleLabel") || undefined}>{body}</div>;
}

function InlineMarkdown({ text, onNavigate }: { text: string; onNavigate: (href: string) => void }): ReactNode {
  const parts: ReactNode[] = [];
  const pattern = /\[([^\]]+)\]\(([^)]+)\)|(`[^`]+`)|\*\*([^*]+)\*\*/g;
  let cursor = 0;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(text)) !== null) {
    if (match.index > cursor) parts.push(text.slice(cursor, match.index));
    const href = match[2] === undefined ? undefined : safeLink(match[2]);
    if (match[1] !== undefined && href !== undefined) parts.push(<button type="button" className="ui-link-button" key={match.index} onClick={() => onNavigate(href)}>{match[1]}</button>);
    else if (match[3] !== undefined) parts.push(<code key={match.index}>{match[3].slice(1, -1)}</code>);
    else if (match[4] !== undefined) parts.push(<strong key={match.index}>{match[4]}</strong>);
    else parts.push(match[0]);
    cursor = pattern.lastIndex;
  }
  if (cursor < text.length) parts.push(text.slice(cursor));
  return parts;
}

function MarkdownView({ source, onNavigate }: { source: string; onNavigate: (href: string) => void }): ReactNode {
  let fenced = false;
  const lines = source.split("\n");
  return (
    <div className="ui-markdown">
      {lines.map((line, index) => {
        if (line.startsWith("```")) {
          fenced = !fenced;
          return <span key={index} />;
        }
        if (fenced) return <pre key={index}><code>{line}</code></pre>;
        const heading = /^(#{1,6})\s+(.+)$/.exec(line);
        if (heading !== null) return React.createElement(`h${heading[1].length}`, { key: index }, <InlineMarkdown text={heading[2]} onNavigate={onNavigate} />);
        const bullet = /^\s*[-*]\s+(.+)$/.exec(line);
        if (bullet !== null) return <div className="ui-markdown-bullet" key={index}>• <InlineMarkdown text={bullet[1]} onNavigate={onNavigate} /></div>;
        return <p key={index}><InlineMarkdown text={line} onNavigate={onNavigate} /></p>;
      })}
    </div>
  );
}

function ContentPrimitive({ node }: { node: UiElementNode }): ReactNode {
  const actions = useRendererActions();
  const props = node.props;
  switch (node.type) {
    case "Text": {
      const content = stringProp(props, "value") || <NodeChildren node={node} />;
      const role = stringProp(props, "role");
      if (role === "heading") return <h3 className={`ui-text tone-${stringProp(props, "tone", "neutral")}`}>{content}</h3>;
      return <span role={role === "status" ? "status" : undefined} className={`ui-text tone-${stringProp(props, "tone", "neutral")}`}>{content}</span>;
    }
    case "Markdown": return <MarkdownView source={stringProp(props, "source")} onNavigate={(href) => actions.dispatch(node, "navigate", eventPayload(node, { href }))} />;
    case "Code": return <pre className="ui-code" aria-label={stringProp(props, "accessibleLabel") || `Code${stringProp(props, "language") ? ` in ${stringProp(props, "language")}` : ""}`}><code>{stringProp(props, "value") || node.children.map((child) => child.kind === "text" ? child.text : "").join("")}</code></pre>;
    case "Diff": {
      const patch = stringProp(props, "patch") || `${stringProp(props, "before")}\n${stringProp(props, "after")}`;
      return <figure className="ui-diff"><figcaption>{stringProp(props, "path", "Changes")}</figcaption><pre>{patch.split("\n").map((line, index) => <span key={index} className={line.startsWith("+") ? "diff-add" : line.startsWith("-") ? "diff-remove" : ""}>{line}{"\n"}</span>)}</pre></figure>;
    }
    case "Image": {
      const alt = stringProp(props, "alt", nodeLabel(node));
      const source = safeMediaSource(stringProp(props, "src"), "image");
      return source === undefined
        ? <div role="img" aria-label={alt} className="ui-media-fallback">🖼 {alt}</div>
        : <figure><img className="ui-image" src={source} alt={alt} /><figcaption>{stringProp(props, "caption")}</figcaption></figure>;
    }
    case "Audio": {
      const source = safeMediaSource(stringProp(props, "src"), "audio");
      const transcript = stringProp(props, "transcript");
      return <figure>{source === undefined ? <div className="ui-media-fallback">Audio unavailable</div> : <audio controls src={source} aria-label={nodeLabel(node)} />}{transcript.length > 0 ? <details><summary>Transcript</summary><p>{transcript}</p></details> : null}</figure>;
    }
    case "JsonTree": return <JsonValue value={props.value ?? null} label={nodeLabel(node)} depth={0} maxDepth={Math.max(0, numberProp(props, "expandedDepth", 2))} />;
    case "LogViewer": return <div className="ui-log" role="log" aria-live="polite"><GenericItems items={arrayValue(props.lines)} /></div>;
    default: return null;
  }
}

function JsonValue({ value, label, depth, maxDepth }: { value: UiJsonValue; label: string; depth: number; maxDepth: number }): ReactNode {
  if (typeof value !== "object" || value === null) return <span>{displayValue(value)}</span>;
  if (depth >= 32) return <span className="ui-muted">[nested value omitted]</span>;
  const allEntries = Array.isArray(value) ? value.map((entry, index) => [String(index), entry] as const) : Object.entries(value);
  const entries = allEntries.slice(0, MAX_JSON_ENTRIES);
  return (
    <details open={depth < maxDepth} className="ui-json">
      <summary>{label} <span className="ui-muted">{Array.isArray(value) ? `[${allEntries.length}]` : `{${allEntries.length}}`}</span></summary>
      <dl>{entries.map(([key, entry]) => <React.Fragment key={key}><dt>{key}</dt><dd><JsonValue value={entry} label={key} depth={depth + 1} maxDepth={maxDepth} /></dd></React.Fragment>)}</dl>
      {allEntries.length > MAX_JSON_ENTRIES ? <p className="ui-muted">{allEntries.length - MAX_JSON_ENTRIES} entries omitted.</p> : null}
    </details>
  );
}

function CollectionPrimitive({ node }: { node: UiElementNode }): ReactNode {
  const actions = useRendererActions();
  const props = node.props;
  if (node.type === "VirtualList") return <VirtualListView node={node} />;
  if (node.type === "Table") {
    const columns = arrayValue(props.columns).map((column) => typeof column === "string" ? { key: column, label: column } : objectValue(column)).filter((column): column is Readonly<Record<string, UiJsonValue>> => column !== undefined);
    const allRows = arrayValue(props.rows);
    const rows = allRows.slice(0, MAX_STATIC_ITEMS);
    return (
      <div className="ui-table-scroll"><table><thead><tr>{columns.map((column) => { const key = String(column.key); const active = props.sortKey === key; const direction = active && props.sortDirection === "descending" ? "ascending" : "descending"; return <th key={key} scope="col" aria-sort={active ? String(props.sortDirection ?? "ascending") as "ascending" | "descending" : "none"}><button className="ui-table-sort" onClick={() => actions.dispatch(node, "select", eventPayload(node, { sortKey: key, sortDirection: direction }))}>{displayValue(column.label ?? key)}</button></th>; })}</tr></thead><tbody>{rows.map((row, index) => { const record = objectValue(row) ?? {}; const selectable = typeof props.selectAction === "string"; const select = (): void => actions.dispatch(node, "select", eventPayload(node, { row, index })); return <tr key={String(record.id ?? record.key ?? index)} tabIndex={selectable ? 0 : undefined} onClick={selectable ? select : undefined} onKeyDown={selectable ? (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); select(); } } : undefined}>{columns.map((column) => <td key={String(column.key)}>{displayValue(record[String(column.key)])}</td>)}</tr>; })}</tbody></table>{allRows.length > MAX_STATIC_ITEMS ? <p className="ui-muted">{allRows.length - MAX_STATIC_ITEMS} rows omitted; use VirtualList for the full dataset.</p> : null}</div>
    );
  }
  if (node.type === "KeyValue") {
    const entries = objectValue(props.entries) ?? {};
    return <dl className="ui-key-value">{Object.entries(entries).map(([key, value]) => <React.Fragment key={key}><dt>{key}</dt><dd>{displayValue(value)}</dd></React.Fragment>)}</dl>;
  }
  if (node.type === "Tree") {
    const expanded = new Set(arrayValue(props.expandedKeys).map(String));
    return <div role="tree" className="ui-tree"><TreeItems node={node} items={arrayValue(props.items)} level={1} expanded={expanded} /></div>;
  }
  if (node.type === "Graph") return <GraphView node={node} />;
  if (node.type === "Chart" || node.type === "Sparkline") return <ChartView node={node} />;
  const items = arrayValue(props.items);
  const empty = stringProp(props, "emptyMessage", "No items");
  if (items.length === 0 && node.children.length === 0) return <p className="ui-muted">{empty}</p>;
  const interactive = typeof props.action === "string" || typeof props.selectAction === "string";
  return <>{items.length > 0 ? React.createElement(node.type === "Timeline" ? "ol" : "ul", { className: "ui-list" }, items.slice(0, MAX_STATIC_ITEMS).map((item, index) => { const record = objectValue(item); const label = record === undefined ? displayValue(item) : displayValue(record.label ?? record.title ?? record.value ?? item); const key = record === undefined ? `${index}-${label}` : String(record.id ?? record.key ?? `${index}-${label}`); return <li key={key}>{interactive ? <button onClick={() => actions.dispatch(node, "select", eventPayload(node, { item, index }))}>{label}</button> : label}</li>; }), items.length > MAX_STATIC_ITEMS ? <li className="ui-muted">{items.length - MAX_STATIC_ITEMS} more items omitted.</li> : null) : null}<NodeChildren node={node} /></>;
}

interface GraphLayoutNode {
  index: number;
  id: string;
  label: string;
  status: string;
  layer: number;
  slot: number;
  x: number;
  y: number;
  width: number;
  item: UiJsonValue;
}

interface GraphLayout {
  nodes: readonly GraphLayoutNode[];
  edges: readonly { from: GraphLayoutNode; to: GraphLayoutNode; label: string }[];
  width: number;
  height: number;
}

const GRAPH_NODE_HEIGHT = 30;
const GRAPH_LAYER_GAP = 56;
const GRAPH_SLOT_GAP = 18;
const GRAPH_CHAR_WIDTH = 7.4;
const GRAPH_MAX_NODES = 400;

function graphItemKey(item: UiJsonValue, index: number): string {
  const record = objectValue(item);
  const id = record === undefined ? undefined : record.id ?? record.key;
  return typeof id === "string" && id.length > 0 ? id : graphItemLabel(item, index);
}

function graphItemLabel(item: UiJsonValue, index: number): string {
  const record = objectValue(item);
  if (record === undefined) return displayValue(item) || `Node ${index + 1}`;
  return displayValue(record.label ?? record.title ?? record.name ?? record.id ?? item) || `Node ${index + 1}`;
}

/**
 * Longest-path layering, mirroring `layout_graph_diagram` in the terminal host
 * so both renderers rank a workflow the same way. Relaxation is bounded by the
 * node count and capped at `nodes - 1`, so a cycle terminates instead of
 * running away; the leftover back-edge is still drawn, just not layered.
 */
function graphLayers(count: number, edges: readonly (readonly [number, number])[]): number[] {
  const depth = new Array<number>(count).fill(0);
  const cap = Math.max(0, count - 1);
  for (let pass = 0; pass < count; pass += 1) {
    let changed = false;
    for (const [from, to] of edges) {
      if (from === to) continue;
      const candidate = depth[from] + 1;
      if (candidate > depth[to] && candidate <= cap) {
        depth[to] = candidate;
        changed = true;
      }
    }
    if (!changed) break;
  }
  const ranks = [...new Set(depth)].sort((left, right) => left - right);
  return depth.map((value) => ranks.indexOf(value));
}

/** `undefined` when the data cannot support a diagram; the caller lists instead. */
function layoutGraph(node: UiElementNode): GraphLayout | undefined {
  const items = arrayValue(node.props.nodes);
  if (items.length === 0 || items.length > GRAPH_MAX_NODES) return undefined;
  const keys = items.map((item, index) => graphItemKey(item, index));
  const indexOf = (key: string): number | undefined => {
    const direct = keys.indexOf(key);
    if (direct >= 0) return direct;
    const byLabel = items.findIndex((item, index) => graphItemLabel(item, index) === key);
    return byLabel >= 0 ? byLabel : undefined;
  };
  const declared = arrayValue(node.props.edges).flatMap((value) => {
    const record = objectValue(value);
    if (record === undefined) return [];
    const from = record.from ?? record.source;
    const to = record.to ?? record.target;
    if (typeof from !== "string" || typeof to !== "string") return [];
    const fromIndex = indexOf(from);
    const toIndex = indexOf(to);
    if (fromIndex === undefined || toIndex === undefined) return [];
    return [{ from: fromIndex, to: toIndex, label: displayValue(record.label ?? record.condition ?? "") }];
  });
  if (declared.length === 0) return undefined;
  const depth = graphLayers(items.length, declared.map((edge) => [edge.from, edge.to] as const));
  const layerCount = Math.max(...depth) + 1;
  if (layerCount < 2) return undefined;
  const horizontal = node.props.direction === "horizontal";
  const slots = new Array<number>(layerCount).fill(0);
  const laid: GraphLayoutNode[] = items.map((item, index) => {
    const label = graphItemLabel(item, index);
    const record = objectValue(item);
    const slot = slots[depth[index]];
    slots[depth[index]] += 1;
    return {
      index, item, label,
      id: keys[index],
      status: typeof record?.status === "string" ? record.status : "",
      layer: depth[index], slot, x: 0, y: 0,
      width: Math.max(64, Math.min(240, Math.round(label.length * GRAPH_CHAR_WIDTH) + 24)),
    };
  });
  const widest = Math.max(...laid.map((entry) => entry.width));
  const lanes = Math.max(...slots);
  for (const entry of laid) {
    const along = entry.layer * ((horizontal ? widest : GRAPH_NODE_HEIGHT) + GRAPH_LAYER_GAP);
    const across = entry.slot * ((horizontal ? GRAPH_NODE_HEIGHT : widest) + GRAPH_SLOT_GAP);
    entry.x = horizontal ? along : across;
    entry.y = horizontal ? across : along;
  }
  const span = (count: number, size: number, gap: number): number => count * size + Math.max(0, count - 1) * gap;
  return {
    nodes: laid,
    edges: declared.map((edge) => ({ from: laid[edge.from], to: laid[edge.to], label: edge.label })),
    width: horizontal ? span(layerCount, widest, GRAPH_LAYER_GAP) : span(lanes, widest, GRAPH_SLOT_GAP),
    height: horizontal ? span(lanes, GRAPH_NODE_HEIGHT, GRAPH_SLOT_GAP) : span(layerCount, GRAPH_NODE_HEIGHT, GRAPH_LAYER_GAP),
  };
}

function GraphNodeList({ node }: { node: UiElementNode }): ReactNode {
  return (
    <>
      <div className="ui-graph-nodes"><GenericItems items={arrayValue(node.props.nodes)} /></div>
      <div className="ui-graph-edges" aria-label="Edges"><GenericItems items={arrayValue(node.props.edges)} /></div>
    </>
  );
}

/**
 * A layered DAG drawn as inline SVG, laid out in the host. Nodes stay
 * individually selectable (a `select` event carries the node id), and the
 * whole diagram degrades to the node/edge lists when the data cannot be
 * ranked — the same fidelity ladder the terminal host walks.
 */
function GraphView({ node }: { node: UiElementNode }): ReactNode {
  const actions = useRendererActions();
  const layout = layoutGraph(node);
  const label = nodeLabel(node);
  if (layout === undefined) {
    return <figure className="ui-graph"><figcaption>{label}</figcaption><GraphNodeList node={node} /></figure>;
  }
  const selected = stringProp(node.props, "selectedKey");
  const select = (entry: GraphLayoutNode): void =>
    actions.dispatch(node, "select", eventPayload(node, { nodeId: entry.id, index: entry.index, item: entry.item }));
  const marker = `${node.id ?? "graph"}-arrow`;
  return (
    <figure className="ui-graph">
      <figcaption>{label}</figcaption>
      <svg
        className="ui-graph-canvas"
        role="group"
        aria-label={`${label}: ${layout.nodes.length} nodes, ${layout.edges.length} connections`}
        viewBox={`-2 -2 ${layout.width + 4} ${layout.height + 4}`}
        style={{ maxWidth: "100%", height: "auto" }}
      >
        <defs>
          <marker id={marker} markerWidth="7" markerHeight="7" refX="6" refY="3" orient="auto">
            <path d="M0,0 L6,3 L0,6 Z" fill="currentColor" />
          </marker>
        </defs>
        <g className="ui-graph-edge-layer" fill="none" stroke="currentColor" strokeWidth="1">
          {layout.edges.map((edge, index) => {
            const start = { x: edge.from.x + edge.from.width / 2, y: edge.from.y + GRAPH_NODE_HEIGHT };
            const end = { x: edge.to.x + edge.to.width / 2, y: edge.to.y };
            const middle = (start.y + end.y) / 2;
            return (
              <path
                key={`${edge.from.id}-${edge.to.id}-${index}`}
                className="ui-graph-edge"
                d={`M${start.x},${start.y} C${start.x},${middle} ${end.x},${middle} ${end.x},${end.y}`}
                markerEnd={`url(#${marker})`}
              >
                <title>{edge.label.length > 0 ? `${edge.from.label} → ${edge.to.label}: ${edge.label}` : `${edge.from.label} → ${edge.to.label}`}</title>
              </path>
            );
          })}
        </g>
        {layout.nodes.map((entry) => (
          <g
            key={entry.id}
            className={`ui-graph-node status-${entry.status || "unknown"}${entry.id === selected ? " is-selected" : ""}`}
            data-ui-graph-node={entry.id}
            role="button"
            tabIndex={0}
            aria-label={entry.status.length > 0 ? `${entry.label}, ${entry.status}` : entry.label}
            aria-pressed={entry.id === selected}
            onClick={() => select(entry)}
            onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); select(entry); } }}
          >
            <rect x={entry.x} y={entry.y} width={entry.width} height={GRAPH_NODE_HEIGHT} rx="4" fill="none" stroke="currentColor" />
            <text x={entry.x + entry.width / 2} y={entry.y + GRAPH_NODE_HEIGHT / 2} textAnchor="middle" dominantBaseline="middle" fill="currentColor">{entry.label}</text>
          </g>
        ))}
      </svg>
      <details className="ui-graph-detail"><summary>Node and edge list</summary><GraphNodeList node={node} /></details>
    </figure>
  );
}

function VirtualListView({ node }: { node: UiElementNode }): ReactNode {
  const actions = useRendererActions();
  const items = arrayValue(node.props.items);
  const rowHeight = Math.max(18, Math.min(200, numberProp(node.props, "rowHeight", 28)));
  const height = Math.max(rowHeight, Math.min(2_000, numberProp(node.props, "height", 320)));
  const overscan = Math.max(1, Math.min(50, numberProp(node.props, "overscan", 4)));
  const [scrollTop, setScrollTop] = useState(0);
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - overscan);
  const end = Math.min(items.length, Math.ceil((scrollTop + height) / rowHeight) + overscan);
  return (
    <div className="ui-virtual-list" role="list" aria-label={nodeLabel(node)} style={{ height, overflow: "auto", position: "relative" }} onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}>
      <div style={{ height: items.length * rowHeight, position: "relative" }}>
        {items.slice(start, end).map((item, offset) => {
          const index = start + offset;
          const record = objectValue(item);
          const label = record === undefined ? displayValue(item) : displayValue(record.label ?? record.title ?? record.value ?? item);
          return <button type="button" role="listitem" aria-posinset={index + 1} aria-setsize={items.length} key={String(record?.id ?? record?.key ?? index)} style={{ position: "absolute", insetInline: 0, top: index * rowHeight, height: rowHeight }} onClick={() => actions.dispatch(node, "select", eventPayload(node, { item, index }))}>{label}</button>;
        })}
      </div>
    </div>
  );
}

function TreeItems({ node, items, level, expanded }: { node: UiElementNode; items: readonly UiJsonValue[]; level: number; expanded: ReadonlySet<string> }): ReactNode {
  const actions = useRendererActions();
  return items.slice(0, MAX_STATIC_ITEMS).map((item, index) => {
    const record = objectValue(item) ?? {};
    const children = arrayValue(record.children);
    const key = String(record.id ?? record.key ?? index);
    const open = expanded.has(key);
    if (children.length === 0) return <div key={key} role="treeitem" aria-level={level}>{displayValue(record.label ?? record.name ?? item)}</div>;
    return <details key={key} open={open} role="treeitem" aria-level={level} onToggle={(event) => actions.dispatch(node, event.currentTarget.open ? "expand" : "collapse", eventPayload(node, { key }))}><summary>{displayValue(record.label ?? record.name ?? key)}</summary><div role="group"><TreeItems node={node} items={children} level={level + 1} expanded={expanded} /></div></details>;
  });
}

function ChartView({ node }: { node: UiElementNode }): ReactNode {
  const props = node.props;
  const source = (node.type === "Sparkline" ? arrayValue(props.values) : arrayValue(props.data)).slice(0, 10_000);
  const yKey = stringProp(props, "yKey", "value");
  const values = source.map((entry) => {
    if (typeof entry === "number") return entry;
    const record = objectValue(entry);
    return typeof record?.[yKey] === "number" ? record[yKey] as number : 0;
  });
  const maximum = Math.max(1, ...values.map((value) => Math.abs(value)));
  const points = values.map((value, index) => `${values.length <= 1 ? 50 : index * 100 / (values.length - 1)},${50 - value * 45 / maximum}`).join(" ");
  return <figure className="ui-chart"><figcaption>{nodeLabel(node)}</figcaption><svg viewBox="0 0 100 55" role="img" aria-label={`${nodeLabel(node)}: ${values.join(", ")}`} preserveAspectRatio="none"><polyline points={points} fill="none" vectorEffect="non-scaling-stroke" /></svg></figure>;
}

function FeedbackPrimitive({ node }: { node: UiElementNode }): ReactNode {
  const props = node.props;
  const tone = stringProp(props, "tone", "neutral");
  const title = stringProp(props, "title") || stringProp(props, "label");
  const message = stringProp(props, "message");
  if (node.type === "Progress") {
    const maximum = Math.max(1, numberProp(props, "maximum", 100));
    const value = Math.min(maximum, Math.max(0, numberProp(props, "value")));
    return <label className="ui-progress">{stringProp(props, "label")}<progress max={maximum} value={booleanProp(props, "indeterminate") ? undefined : value} /></label>;
  }
  if (node.type === "Spinner") return <span className="ui-spinner" role="status" aria-label={stringProp(props, "label", "Loading")}>◌</span>;
  if (node.type === "Badge") return <span className={`ui-badge tone-${tone}`}>{title || message}<NodeChildren node={node} /></span>;
  const role = node.type === "Alert" || node.type === "ErrorBoundary" ? "alert" : node.type === "Toast" ? "status" : undefined;
  return <section role={role} className={`ui-feedback tone-${tone}`}><strong>{title}</strong>{message.length > 0 ? <p>{message}</p> : null}<NodeChildren node={node} /></section>;
}

function NavigationPrimitive({ node }: { node: UiElementNode }): ReactNode {
  const actions = useRendererActions();
  const props = node.props;
  const items = arrayValue(props.items);
  if (node.type === "Tabs") {
    const tabs = arrayValue(props.tabs);
    const active = stringProp(props, "activeId");
    return <section className="ui-tabs"><div role="tablist">{tabs.map((tab, index) => { const record = objectValue(tab) ?? {}; const id = String(record.id ?? index); return <button key={id} role="tab" aria-selected={id === active} disabled={record.disabled === true} tabIndex={id === active ? 0 : -1} onClick={() => actions.dispatch(node, "change", eventPayload(node, { activeId: id }))} onKeyDown={(event) => tabKeyNavigation(event)}>{displayValue(record.label ?? id)}</button>; })}</div><div role="tabpanel"><NodeChildren node={node} /></div></section>;
  }
  if (node.type === "Details") {
    return <details open={booleanProp(props, "open")} onToggle={(event) => actions.dispatch(node, (event.currentTarget as HTMLDetailsElement).open ? "expand" : "collapse", eventPayload(node))}><summary>{stringProp(props, "title", nodeLabel(node))}</summary><NodeChildren node={node} /></details>;
  }
  if (node.type === "Pagination") {
    const current = numberProp(props, "current", 1);
    const total = Math.max(1, numberProp(props, "total", 1));
    return <nav aria-label={nodeLabel(node)} className="ui-pagination"><button disabled={current <= 1} onClick={() => actions.dispatch(node, "navigate", eventPayload(node, { page: current - 1 }))}>Previous</button><span>{current} / {total}</span><button disabled={current >= total} onClick={() => actions.dispatch(node, "navigate", eventPayload(node, { page: current + 1 }))}>Next</button></nav>;
  }
  if (node.type === "Link") {
    const label = stringProp(props, "label", nodeLabel(node));
    const href = safeLink(stringProp(props, "href"));
    return href === undefined
      ? <button className="ui-link-button" onClick={() => actions.dispatch(node, "navigate", eventPayload(node))}>{label}<NodeChildren node={node} /></button>
      : <button type="button" className="ui-link-button" onClick={(event) => actions.dispatch(node, "navigate", eventPayload(node, { href }), modifiers(event.nativeEvent))}>{label}<NodeChildren node={node} /></button>;
  }
  const tag = node.type === "Breadcrumb" ? "nav" : "div";
  const containerRole = node.type === "Menu" || node.type === "ActionMenu" || node.type === "ContextMenu"
    ? "menu"
    : node.type === "Toolbar"
      ? "toolbar"
      : node.type === "CommandList"
        ? "listbox"
        : undefined;
  const itemRole = containerRole === "menu" ? "menuitem" : containerRole === "listbox" ? "option" : undefined;
  const content = items.map((item, index) => {
    const record = objectValue(item) ?? {};
    const label = displayValue(record.label ?? record.title ?? item);
    const disabled = record.disabled === true;
    const action = typeof record.action === "string" ? record.action : stringProp(props, "action");
    return <button role={itemRole} key={String(record.id ?? record.key ?? index)} disabled={disabled} onClick={() => actions.dispatch(node, "action", { action, item })}>{label}</button>;
  });
  return React.createElement(tag, { className: `ui-navigation ui-${node.type.toLowerCase()}`, role: containerRole, "aria-label": nodeLabel(node) }, content, <NodeChildren node={node} />);
}

function tabKeyNavigation(event: ReactKeyboardEvent<HTMLButtonElement>): void {
  if (event.key !== "ArrowLeft" && event.key !== "ArrowRight" && event.key !== "Home" && event.key !== "End") return;
  const tabs = Array.from(event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role=tab]:not(:disabled)") ?? []);
  const current = tabs.indexOf(event.currentTarget);
  const next = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : (current + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
  event.preventDefault();
  tabs[next]?.focus();
}

function useInputValue(node: UiElementNode): [UiJsonValue, (value: UiJsonValue) => void] {
  const external = node.props.value
    ?? (typeof node.props.checked === "boolean" ? node.props.checked : undefined)
    ?? node.props.defaultValue
    ?? "";
  const [value, setValue] = useState<UiJsonValue>(external);
  useEffect(() => setValue(external), [external]);
  return [value, setValue];
}

function InputPrimitive({ node }: { node: UiElementNode }): ReactNode {
  const actions = useRendererActions();
  const props = node.props;
  const name = stringProp(props, "name", node.id ?? node.type);
  const label = nodeLabel(node);
  const disabled = booleanProp(props, "disabled");
  const readOnly = booleanProp(props, "readOnly");
  const [value, setValue] = useInputValue(node);
  const change = (next: UiJsonValue, event?: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>): void => {
    setValue(next);
    actions.dispatch(node, "change", eventPayload(node, { name, value: next }), event === undefined ? undefined : modifiers(event.nativeEvent));
  };
  if (node.type === "TextInput" || node.type === "TextArea") {
    const common = { name, value: displayValue(value), disabled, readOnly, required: booleanProp(props, "required"), placeholder: stringProp(props, "placeholder"), "aria-label": label, onFocus: () => actions.dispatch(node, "focus"), onBlur: () => actions.dispatch(node, "blur"), onChange: (event: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => change(event.target.value, event) };
    const requestedType = stringProp(props, "inputType", "text").toLowerCase();
    const safeType = ["text", "email", "url", "search", "number", "tel"].includes(requestedType) ? requestedType : "text";
    return <label className="ui-field"><span>{label}</span>{node.type === "TextArea" ? <textarea {...common} rows={Math.max(1, numberProp(props, "rows", 4))} /> : <input {...common} type={safeType} />}</label>;
  }
  if (node.type === "Checkbox" || node.type === "Radio") {
    const checked = typeof value === "boolean" ? value : props.checked === true || value === "true";
    return <label className="ui-choice"><input type={node.type === "Radio" ? "radio" : "checkbox"} name={name} value={stringProp(props, "optionValue", "true")} checked={checked} disabled={disabled} onChange={(event) => change(event.target.checked, event)} />{label}</label>;
  }
  const options = arrayValue(props.options);
  const multiple = node.type === "MultiSelect";
  return <label className="ui-field"><span>{label}</span><select name={name} value={multiple ? (Array.isArray(value) ? value.map(String) : []) : displayValue(value)} multiple={multiple} disabled={disabled} required={booleanProp(props, "required")} onChange={(event) => change(multiple ? Array.from(event.currentTarget.selectedOptions).map((option) => option.value) : event.currentTarget.value, event)}>{options.map((option, index) => { const record = objectValue(option) ?? {}; const optionValue = displayValue(record.value ?? option); return <option key={String(record.id ?? optionValue ?? index)} value={optionValue} disabled={record.disabled === true}>{displayValue(record.label ?? optionValue)}</option>; })}</select></label>;
}

function FormPrimitive({ node }: { node: UiElementNode }): ReactNode {
  const actions = useRendererActions();
  const onSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    actions.dispatch(node, "submit", eventPayload(node, { formData: formObject(event.currentTarget) }));
  };
  return <form className="ui-form" aria-label={nodeLabel(node)} onSubmit={onSubmit} onReset={() => actions.dispatch(node, "action")}><NodeChildren node={node} /></form>;
}

function ActionPrimitive({ node }: { node: UiElementNode }): ReactNode {
  const actions = useRendererActions();
  const props = node.props;
  if (node.type === "Button") {
    const submit = props.submit === true;
    return <button type={submit ? "submit" : "button"} className={`ui-button tone-${stringProp(props, "tone", "neutral")}`} disabled={booleanProp(props, "disabled")} aria-keyshortcuts={stringProp(props, "shortcut") || undefined} onClick={submit ? undefined : (event) => actions.dispatch(node, activationEvent(node), undefined, modifiers(event.nativeEvent))}>{stringProp(props, "label") || <NodeChildren node={node} />}</button>;
  }
  return <NavigationPrimitive node={node} />;
}

function DomainPrimitive({ node }: { node: UiElementNode }): ReactNode {
  const status = stringProp(node.props, "status");
  const data = node.props.data;
  return <article className={`ui-domain-card ui-${node.type.toLowerCase()}`} aria-label={nodeLabel(node)}><header><strong>{stringProp(node.props, "title", node.type)}</strong>{status.length > 0 ? <span className="ui-badge">{status}</span> : null}</header><NodeChildren node={node} />{data === undefined ? null : <JsonValue value={data} label="Details" depth={0} maxDepth={1} />}</article>;
}

const LAYOUT = new Set(["Box", "Stack", "Row", "Grid", "Split", "Spacer", "ScrollArea"]);
const CONTENT = new Set(["Text", "Markdown", "Code", "Diff", "Image", "Audio", "JsonTree", "LogViewer"]);
const COLLECTION = new Set(["VirtualList", "List", "Table", "Tree", "KeyValue", "Timeline", "Graph", "Chart", "Sparkline"]);
const FEEDBACK = new Set(["Badge", "Progress", "Spinner", "Alert", "Toast", "EmptyState", "ErrorBoundary"]);
const NAVIGATION = new Set(["Tabs", "Breadcrumb", "Menu", "CommandList", "Pagination", "Link", "Details"]);
const INPUT = new Set(["TextInput", "TextArea", "Select", "MultiSelect", "Checkbox", "Radio"]);
const ACTION = new Set(["Button", "ActionMenu", "Toolbar", "ContextMenu"]);
const DOMAIN = new Set(["ToolCard", "ArtifactCard", "ApprovalCard", "AgentCard", "WorkflowNode", "PatchCard", "TestReport", "PermissionDiff", "TraceView", "CostView"]);

function isNodeCapabilitySatisfied(node: UiElementNode, capabilities: UiCapabilities): boolean {
  if (node.requires !== undefined && node.requires.length > 0) {
    const unmet = node.requires.filter((req) => !req.optional && !supportsRequirement(capabilities, req));
    if (unmet.length > 0) return false;
  }
  if (capabilities.primitives !== "*" && !capabilities.primitives.includes(node.type)) {
    if (node.type !== "TerminalOnly" && node.type !== "WebOnly") return false;
  }
  return true;
}

function RemoteElement({ node }: { node: UiElementNode }): ReactNode {
  const meta = useRendererMeta();
  if (node.props.hidden === true) return null;

  // Check capability requirements
  if (!isNodeCapabilitySatisfied(node, meta.capabilities)) {
    if (node.fallback !== undefined) {
      return <RemoteNode node={node.fallback} />;
    }
    return (
      <section className="ui-unsupported" role="note">
        {nodeLabel(node)} <span className="ui-muted">({node.type} requires unavailable capability)</span>
        <NodeChildren node={node} />
      </section>
    );
  }

  let content: ReactNode;
  if (LAYOUT.has(node.type)) content = <LayoutPrimitive node={node} />;
  else if (CONTENT.has(node.type)) content = <ContentPrimitive node={node} />;
  else if (COLLECTION.has(node.type)) content = <CollectionPrimitive node={node} />;
  else if (FEEDBACK.has(node.type)) content = <FeedbackPrimitive node={node} />;
  else if (NAVIGATION.has(node.type)) content = <NavigationPrimitive node={node} />;
  else if (INPUT.has(node.type)) content = <InputPrimitive node={node} />;
  else if (node.type === "Form") content = <FormPrimitive node={node} />;
  else if (ACTION.has(node.type)) content = <ActionPrimitive node={node} />;
  else if (DOMAIN.has(node.type)) content = <DomainPrimitive node={node} />;
  else if (node.type === "WebOnly" || node.type === "TerminalOnly") content = <NodeChildren node={node} />;
  else content = node.fallback === undefined
    ? <section className="ui-unsupported" role="note">{nodeLabel(node)} <span className="ui-muted">({node.type} is not supported)</span><NodeChildren node={node} /></section>
    : <RemoteNode node={node.fallback} />;
  return <div data-ui-node-id={node.id} data-ui-primitive={node.type} onFocusCapture={() => { meta.activeNodeId.current = node.id; }}>{content}</div>;
}

function RemoteNodeComponent({ node }: { node: UiNode }): ReactNode {
  if (node === null || typeof node !== "object") return null;
  if (node.kind === "text") return <React.Fragment>{node.text ?? ""}</React.Fragment>;
  if (node.kind === "element") return <RemoteElement node={node} />;
  const raw = node as Record<string, unknown>;
  const label = typeof raw.text === "string" ? raw.text : typeof raw.label === "string" ? raw.label : typeof raw.kind === "string" ? `[Unknown node: ${raw.kind}]` : "[Unknown node]";
  return <span className="ui-unsupported" role="note">{label}</span>;
}

const RemoteNode = memo(RemoteNodeComponent);

function collectShortcuts(node: UiNode, output = new Map<string, UiElementNode>()): Map<string, UiElementNode> {
  if (!node || typeof node !== "object" || node.kind !== "element") return output;
  const shortcut = stringProp(node.props, "shortcut").toLowerCase();
  // First declaration wins deterministically within a focused document. A
  // producer cannot shadow an earlier intent by appending a duplicate claim.
  if (shortcut.length > 0 && !output.has(shortcut)) output.set(shortcut, node);
  if (Array.isArray(node.children)) {
    node.children.forEach((child) => collectShortcuts(child, output));
  }
  return output;
}

function shortcutMatches(shortcut: string, event: KeyboardEvent): boolean {
  const parts = shortcut.toLowerCase().split("+").map((part) => part.trim());
  const key = parts.at(-1);
  return key === event.key.toLowerCase()
    && parts.includes("ctrl") === event.ctrlKey
    && parts.includes("alt") === event.altKey
    && parts.includes("shift") === event.shiftKey
    && (parts.includes("meta") || parts.includes("cmd")) === event.metaKey;
}

function sanitizeEventPayload(payload?: UiJsonValue): UiJsonValue | undefined {
  if (payload === undefined) return undefined;
  try {
    const serialized = JSON.stringify(payload);
    return serialized === undefined ? undefined : JSON.parse(serialized);
  } catch {
    return { error: "Invalid event payload" };
  }
}

function RemoteDocument({ mount, emit, showTerminalFallback, capabilities }: {
  mount: MountedRemoteUi;
  emit: (event: UiEvent) => void;
  showTerminalFallback: boolean;
  capabilities: UiCapabilities;
}): ReactNode {
  const document = mount.projected;
  const activeNodeId = useRef<string | undefined>(undefined);
  const dispatch = useCallback((target: UiElementNode, type: UiEvent["type"], payload?: UiJsonValue, eventModifiers?: UiEventModifiers): void => {
    if (target.id === undefined) return;
    const safePayload = sanitizeEventPayload(payload);
    emit({
      protocolVersion: UI_PROTOCOL_VERSION,
      eventId: typeof crypto !== "undefined" && typeof crypto.randomUUID === "function" ? crypto.randomUUID() : `event-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      documentId: document.documentId,
      revision: document.revision,
      targetId: target.id,
      type,
      ...(safePayload === undefined ? {} : { payload: safePayload }),
      ...(eventModifiers === undefined ? {} : { modifiers: eventModifiers }),
      timestamp: new Date().toISOString(),
    });
  }, [document.documentId, document.revision, emit]);

  useLayoutEffect(() => {
    if (activeNodeId.current === undefined) return;
    if (typeof globalThis.document === "undefined") return;
    const selector = `[data-ui-document-id="${CSS.escape(document.documentId)}"] [data-ui-node-id="${CSS.escape(activeNodeId.current)}"]`;
    const container = globalThis.document.querySelector<HTMLElement>(selector);
    if (container !== null && !container.contains(globalThis.document.activeElement)) {
      container.querySelector<HTMLElement>("button, input, textarea, select, a, [tabindex]")?.focus({ preventScroll: true });
    }
  }, [document.documentId, document.revision]);

  const actions = useMemo<RendererActions>(() => ({ dispatch }), [dispatch]);
  const meta = useMemo<RendererMeta>(() => ({ activeNodeId, capabilities }), [capabilities]);
  const terminalText = showTerminalFallback
    ? toAccessibleText(projectDocument(mount.document, MINIMAL_TERMINAL_CAPABILITIES).root)
    : "";

  return (
    <RendererMetaContext.Provider value={meta}>
      <RendererActionsContext.Provider value={actions}>
        <section className="ui-document" data-ui-document-id={document.documentId} data-ui-revision={document.revision} aria-label={document.metadata?.title ?? "Extension surface"}>
          <header className="ui-extension-chrome" aria-label="Extension identity">
            <span>Extension surface</span>
            <strong>{mount.placement.extensionId ?? "Unidentified extension"}</strong>
            {mount.placement.publisher === undefined ? null : <span>{mount.placement.publisher}</span>}
            {mount.placement.trust === undefined ? null : <span>{mount.placement.trust}</span>}
          </header>
          <RemoteNode node={document.root} />
          {showTerminalFallback ? <details className="ui-terminal-preview"><summary>Terminal fallback</summary><pre>{terminalText}</pre></details> : null}
        </section>
      </RendererActionsContext.Provider>
    </RendererMetaContext.Provider>
  );
}

function ContributionGroup({ definition, mounts, dispatch, recover, showTerminalFallback, activeOverlay, capabilities }: {
  definition: WebSlotDefinition;
  mounts: readonly MountedRemoteUi[];
  dispatch: (event: UiEvent) => void;
  recover?: (request: RemoteUiRecoveryRequest) => void;
  showTerminalFallback: boolean;
  activeOverlay?: boolean;
  capabilities: UiCapabilities;
}): ReactNode {
  const containerRef = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    if (definition.focusManaged !== true || activeOverlay !== true) return;
    if (typeof globalThis.document === "undefined") return;
    const previouslyFocused = globalThis.document.activeElement instanceof HTMLElement
      ? globalThis.document.activeElement
      : undefined;
    const preferred = containerRef.current?.querySelector<HTMLElement>("button, input, textarea, select, a, [tabindex]:not([tabindex='-1'])");
    (preferred ?? containerRef.current)?.focus({ preventScroll: true });
    return () => {
      if (previouslyFocused?.isConnected === true) previouslyFocused.focus({ preventScroll: true });
    };
  }, [activeOverlay, definition.focusManaged]);

  const documents = mounts.map((mount) => (
    <RemoteDocumentBoundary
      key={mount.document.documentId}
      documentId={mount.document.documentId}
      revision={mount.document.revision}
      {...(mount.placement.extensionId === undefined ? {} : { extensionId: mount.placement.extensionId })}
      {...(recover === undefined ? {} : { recover })}
    >
      <RemoteDocument mount={mount} emit={dispatch} showTerminalFallback={showTerminalFallback} capabilities={capabilities} />
    </RemoteDocumentBoundary>
  ));

  return React.createElement(definition.element, {
    ref: containerRef,
    className: `ui-contribution-group ui-slot-${definition.point}`,
    "data-ui-contribution-point": definition.point,
    "data-ui-slot-adapter": definition.point,
    "data-ui-slot-order": definition.order,
    "aria-label": definition.label,
    ...(definition.role === undefined ? {} : { role: definition.role }),
    ...(definition.ariaLive === undefined ? {} : { "aria-live": definition.ariaLive }),
    ...(definition.overlay === true ? { tabIndex: -1 } : {}),
    ...(definition.focusManaged === true && activeOverlay !== true ? { inert: true, "aria-hidden": true } : {}),
  }, documents);
}

const REGION_ORDER: readonly WebHostRegion[] = ["sidebar", "navigation", "primary", "transcript", "composer", "setup", "status", "overlay"];
const REGION_LABELS: Readonly<Record<WebHostRegion, string>> = {
  sidebar: "Extension sidebar region",
  navigation: "Extension navigation region",
  primary: "Extension primary region",
  transcript: "Extension transcript region",
  composer: "Extension composer region",
  setup: "Extension setup region",
  status: "Extension status region",
  overlay: "Extension overlay region",
};

function HostRegion({ region, children }: { region: WebHostRegion; children: ReactNode }): ReactNode {
  const common = {
    className: `ui-host-region ui-host-region-${region}`,
    "data-ui-host-region": region,
    "aria-label": REGION_LABELS[region],
  };
  switch (region) {
    case "sidebar": return <aside {...common}>{children}</aside>;
    case "navigation": return <nav {...common}>{children}</nav>;
    case "primary": return <main {...common}>{children}</main>;
    case "transcript": return <section {...common}>{children}</section>;
    case "composer": return <section {...common}>{children}</section>;
    case "setup": return <section {...common}>{children}</section>;
    case "status": return <footer {...common}>{children}</footer>;
    case "overlay": return <div {...common} aria-live="polite">{children}</div>;
  }
}

export function RemoteUiRenderer({ store, capabilities, dispatch, recover, showTerminalFallback = false }: RemoteUiRendererProps): ReactNode {
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
  const shortcutClaims = useMemo(() => new Map(snapshot.mounts.map((mount) => [
    mount.document.documentId,
    { mount, shortcuts: collectShortcuts(mount.projected.root) },
  ])), [snapshot.mounts]);

  useEffect(() => {
    const listener = (event: KeyboardEvent): void => {
      if (!(event.target instanceof Element)
        || event.target.matches("input, textarea, select, [contenteditable=true]")) return;
      const container = event.target.closest<HTMLElement>("[data-ui-document-id]");
      if (container === null || container.hidden || container.closest("[aria-hidden=true]") !== null) return;
      const style = globalThis.getComputedStyle(container);
      if (style.display === "none" || style.visibility === "hidden") return;
      const documentId = container.dataset.uiDocumentId;
      if (documentId === undefined) return;
      const claim = shortcutClaims.get(documentId);
      if (claim === undefined) return;
      for (const [shortcut, target] of claim.shortcuts) {
        if (!shortcutMatches(shortcut, event) || target.id === undefined) continue;
        event.preventDefault();
        event.stopImmediatePropagation();
        dispatch({
          protocolVersion: UI_PROTOCOL_VERSION,
          eventId: typeof crypto !== "undefined" && typeof crypto.randomUUID === "function" ? crypto.randomUUID() : `event-${Date.now()}-${Math.random().toString(36).slice(2)}`,
          documentId,
          revision: claim.mount.document.revision,
          targetId: target.id,
          type: activationEvent(target),
          modifiers: modifiers(event),
          timestamp: new Date().toISOString(),
        });
        return;
      }
    };
    if (typeof window !== "undefined") {
      window.addEventListener("keydown", listener);
      return () => window.removeEventListener("keydown", listener);
    }
    return () => {};
  }, [dispatch, shortcutClaims]);

  const grouped = useMemo(() => {
    const groups = new Map<WebHostRegion, { definition: WebSlotDefinition; mounts: MountedRemoteUi[] }[]>();
    for (const mount of snapshot.mounts) {
      let definition = webSlotDefinition(mount.placement.point);
      if (definition === undefined) {
        // Fallback for unregistered / custom contribution point: mount into primary section safely
        definition = {
          point: mount.placement.point,
          region: "primary",
          order: 999,
          element: "section",
          label: `Extension slot: ${mount.placement.point}`,
        };
      }
      const region = groups.get(definition.region) ?? [];
      let slot = region.find((candidate) => candidate.definition.point === definition.point);
      if (slot === undefined) {
        slot = { definition, mounts: [] };
        region.push(slot);
        groups.set(definition.region, region);
      }
      slot.mounts.push(mount);
    }
    groups.forEach((slots) => slots.sort((left, right) => left.definition.order - right.definition.order));
    return groups;
  }, [snapshot.mounts]);

  return (
    <div className="remote-ui-root" data-ui-client={capabilities.client}>
      <div className="ui-host-errors" aria-live="polite">
        {Object.entries(snapshot.errors).map(([id, message]) => {
          const extensionId = snapshot.mounts.find((mount) => mount.document.documentId === id)?.placement.extensionId;
          return <RecoveryCard key={id} request={{ documentId: id, ...(extensionId === undefined ? {} : { extensionId }), message }} {...(recover === undefined ? {} : { recover })} />;
        })}
      </div>
      <div className="ui-host-shell">
        {REGION_ORDER.map((region) => {
          const slots = grouped.get(region);
          if (slots === undefined || slots.length === 0) return null;
          const activeOverlay = region === "overlay"
            ? slots.filter(({ definition }) => definition.focusManaged === true).at(-1)?.definition.point
            : undefined;
          return <HostRegion key={region} region={region}>{slots.map(({ definition, mounts }) => (
            <ContributionGroup
              key={definition.point}
              definition={definition}
              mounts={mounts}
              dispatch={dispatch}
              {...(recover === undefined ? {} : { recover })}
              showTerminalFallback={showTerminalFallback}
              {...(definition.focusManaged !== true ? {} : { activeOverlay: definition.point === activeOverlay })}
              capabilities={capabilities}
            />
          ))}</HostRegion>;
        })}
      </div>
    </div>
  );
}
