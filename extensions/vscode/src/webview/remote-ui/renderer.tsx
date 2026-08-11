import React, {
  createContext,
  memo,
  use,
  useCallback,
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
import {
  MINIMAL_TERMINAL_CAPABILITIES,
  UI_PROTOCOL_VERSION,
  projectDocument,
  toAccessibleText,
  type UiCapabilities,
  type UiElementNode,
  type UiEvent,
  type UiEventModifiers,
  type UiJsonValue,
  type UiNode,
  type UiProps,
} from "@codypendent/ui";

import type { MountedRemoteUi, RemoteUiStore } from "./store.js";

export interface RemoteUiRendererProps {
  store: RemoteUiStore;
  capabilities: UiCapabilities;
  dispatch: (event: UiEvent) => void;
  showTerminalFallback?: boolean;
}

interface RendererActions {
  dispatch: (target: UiElementNode, type: UiEvent["type"], payload?: UiJsonValue, modifiers?: UiEventModifiers) => void;
}

interface RendererMeta {
  activeNodeId: React.MutableRefObject<string | undefined>;
}

const RendererActionsContext = createContext<RendererActions | null>(null);
const RendererMetaContext = createContext<RendererMeta | null>(null);

function useRendererActions(): RendererActions {
  const value = use(RendererActionsContext);
  if (value === null) throw new Error("Remote UI primitive rendered outside its document provider");
  return value;
}

function useRendererMeta(): RendererMeta {
  const value = use(RendererMetaContext);
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
  return JSON.stringify(value, null, 2);
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
  for (const [name, raw] of new FormData(form)) {
    if (typeof raw !== "string") continue;
    const current = output[name];
    if (current === undefined) output[name] = raw;
    else if (Array.isArray(current)) output[name] = [...current, raw];
    else output[name] = [current, raw];
  }
  return output;
}

function safeMediaSource(value: string, kind: "image" | "audio"): string | undefined {
  if (value.length > 2_000_000) return undefined;
  if (value.startsWith("vscode-webview-resource:") || value.startsWith("blob:")) return value;
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
  return node.children.map((child, index) => <RemoteNode key={child.id ?? `${node.id ?? node.type}-${index}`} node={child} />);
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
    const columns = Math.max(1, Math.min(24, numberProp(props, "columns", 2)));
    style.gridTemplateColumns = `repeat(${columns}, minmax(0, 1fr))`;
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
  if (node.type === "Graph") {
    return <figure className="ui-graph"><figcaption>{nodeLabel(node)}</figcaption><div className="ui-graph-nodes"><GenericItems items={arrayValue(props.nodes)} /></div><div className="ui-graph-edges" aria-label="Edges"><GenericItems items={arrayValue(props.edges)} /></div></figure>;
  }
  if (node.type === "Chart" || node.type === "Sparkline") return <ChartView node={node} />;
  const items = arrayValue(props.items);
  const empty = stringProp(props, "emptyMessage", "No items");
  if (items.length === 0 && node.children.length === 0) return <p className="ui-muted">{empty}</p>;
  const interactive = typeof props.action === "string" || typeof props.selectAction === "string";
  return <>{items.length > 0 ? React.createElement(node.type === "Timeline" ? "ol" : "ul", { className: "ui-list" }, items.slice(0, MAX_STATIC_ITEMS).map((item, index) => { const record = objectValue(item); const label = record === undefined ? displayValue(item) : displayValue(record.label ?? record.title ?? record.value ?? item); const key = record === undefined ? `${index}-${label}` : String(record.id ?? record.key ?? `${index}-${label}`); return <li key={key}>{interactive ? <button onClick={() => actions.dispatch(node, "select", eventPayload(node, { item, index }))}>{label}</button> : label}</li>; }), items.length > MAX_STATIC_ITEMS ? <li className="ui-muted">{items.length - MAX_STATIC_ITEMS} more items omitted.</li> : null) : null}<NodeChildren node={node} /></>;
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
  const tabs = [...(event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role=tab]:not(:disabled)") ?? [])];
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
  return <label className="ui-field"><span>{label}</span><select name={name} value={multiple ? (Array.isArray(value) ? value.map(String) : []) : displayValue(value)} multiple={multiple} disabled={disabled} required={booleanProp(props, "required")} onChange={(event) => change(multiple ? [...event.currentTarget.selectedOptions].map((option) => option.value) : event.currentTarget.value, event)}>{options.map((option, index) => { const record = objectValue(option) ?? {}; const optionValue = displayValue(record.value ?? option); return <option key={String(record.id ?? optionValue ?? index)} value={optionValue} disabled={record.disabled === true}>{displayValue(record.label ?? optionValue)}</option>; })}</select></label>;
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

function RemoteElement({ node }: { node: UiElementNode }): ReactNode {
  const meta = useRendererMeta();
  if (node.props.hidden === true) return null;
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
  else content = node.fallback === undefined ? <section className="ui-unsupported" role="note">{nodeLabel(node)} <span className="ui-muted">({node.type} is not supported)</span><NodeChildren node={node} /></section> : <RemoteNode node={node.fallback} />;
  return <div data-ui-node-id={node.id} data-ui-primitive={node.type} onFocusCapture={() => { meta.activeNodeId.current = node.id; }}>{content}</div>;
}

function RemoteNodeComponent({ node }: { node: UiNode }): ReactNode {
  if (node.kind === "text") return <React.Fragment>{node.text}</React.Fragment>;
  return <RemoteElement node={node} />;
}

const RemoteNode = memo(RemoteNodeComponent);

function collectShortcuts(node: UiNode, output = new Map<string, UiElementNode>()): Map<string, UiElementNode> {
  if (node.kind === "text") return output;
  const shortcut = stringProp(node.props, "shortcut").toLowerCase();
  // First declaration wins deterministically within a focused document. A
  // producer cannot shadow an earlier intent by appending a duplicate claim.
  if (shortcut.length > 0 && !output.has(shortcut)) output.set(shortcut, node);
  node.children.forEach((child) => collectShortcuts(child, output));
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

function RemoteDocument({ mount, emit, showTerminalFallback }: { mount: MountedRemoteUi; emit: (event: UiEvent) => void; showTerminalFallback: boolean }): ReactNode {
  const document = mount.projected;
  const activeNodeId = useRef<string | undefined>(undefined);
  const dispatch = useCallback((target: UiElementNode, type: UiEvent["type"], payload?: UiJsonValue, eventModifiers?: UiEventModifiers): void => {
    if (target.id === undefined) return;
    emit({
      protocolVersion: UI_PROTOCOL_VERSION,
      eventId: crypto.randomUUID(),
      documentId: document.documentId,
      revision: document.revision,
      targetId: target.id,
      type,
      ...(payload === undefined ? {} : { payload }),
      ...(eventModifiers === undefined ? {} : { modifiers: eventModifiers }),
      timestamp: new Date().toISOString(),
    });
  }, [document.documentId, document.revision, emit]);
  useLayoutEffect(() => {
    if (activeNodeId.current === undefined) return;
    const selector = `[data-ui-document-id="${CSS.escape(document.documentId)}"] [data-ui-node-id="${CSS.escape(activeNodeId.current)}"]`;
    const container = globalThis.document.querySelector<HTMLElement>(selector);
    if (container !== null && !container.contains(globalThis.document.activeElement)) {
      container.querySelector<HTMLElement>("button, input, textarea, select, a, [tabindex]")?.focus({ preventScroll: true });
    }
  }, [document.documentId, document.revision]);
  const actions = useMemo<RendererActions>(() => ({ dispatch }), [dispatch]);
  const meta = useMemo<RendererMeta>(() => ({ activeNodeId }), []);
  const terminalText = showTerminalFallback
    ? toAccessibleText(projectDocument(mount.document, MINIMAL_TERMINAL_CAPABILITIES).root)
    : "";
  return (
    <RendererMetaContext value={meta}>
      <RendererActionsContext value={actions}>
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
      </RendererActionsContext>
    </RendererMetaContext>
  );
}

function ContributionGroup({ point, mounts, dispatch, showTerminalFallback }: { point: string; mounts: readonly MountedRemoteUi[]; dispatch: (event: UiEvent) => void; showTerminalFallback: boolean }): ReactNode {
  const documents = mounts.map((mount) => <RemoteDocument key={mount.document.documentId} mount={mount} emit={dispatch} showTerminalFallback={showTerminalFallback} />);
  const common = { className: `ui-contribution-group ui-slot-${point}`, "data-ui-contribution-point": point, "data-ui-slot-adapter": point };
  switch (point) {
    case "sidebar": return <aside {...common} aria-label="Extension sidebar">{documents}</aside>;
    case "status-item": return <footer {...common} role="status" aria-label="Extension status items">{documents}</footer>;
    case "command": return <nav {...common} aria-label="Extension commands">{documents}</nav>;
    case "command-palette": return <section {...common} role="dialog" aria-label="Extension command palette">{documents}</section>;
    case "quick-pick": return <section {...common} role="listbox" aria-label="Extension quick picks">{documents}</section>;
    case "notification": return <aside {...common} aria-live="polite" aria-label="Extension notifications">{documents}</aside>;
    case "composer-accessory": return <section {...common} aria-label="Extension composer accessories">{documents}</section>;
    case "message-renderer": return <section {...common} role="feed" aria-label="Extension transcript entries">{documents}</section>;
    case "tool-renderer": return <article {...common} aria-label="Extension tool results">{documents}</article>;
    case "artifact-renderer": return <article {...common} aria-label="Extension artifact renderers">{documents}</article>;
    case "workflow-inspector": return <section {...common} aria-label="Extension workflow nodes">{documents}</section>;
    case "blackboard-renderer": return <section {...common} aria-label="Extension blackboard">{documents}</section>;
    case "document-block": return <article {...common} aria-label="Extension document blocks">{documents}</article>;
    case "code-graph-node": return <section {...common} aria-label="Extension code graph nodes">{documents}</section>;
    case "settings-section": return <section {...common} aria-label="Extension settings">{documents}</section>;
    case "setup-step": return <section {...common} aria-label="Extension setup steps">{documents}</section>;
    case "form": return <section {...common} role="form" aria-label="Extension forms">{documents}</section>;
    case "wizard": return <section {...common} role="dialog" aria-label="Extension wizards">{documents}</section>;
    case "dashboard-card": return <section {...common} aria-label="Extension dashboard cards">{documents}</section>;
    case "trace-span-renderer": return <article {...common} aria-label="Extension trace spans">{documents}</article>;
    case "context-menu": return <section {...common} role="menu" aria-label="Extension context menu">{documents}</section>;
    default: return <section {...common} aria-label={`${point} extensions`}>{documents}</section>;
  }
}

export function RemoteUiRenderer({ store, capabilities, dispatch, showTerminalFallback = false }: RemoteUiRendererProps): ReactNode {
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
          eventId: crypto.randomUUID(),
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
    window.addEventListener("keydown", listener);
    return () => window.removeEventListener("keydown", listener);
  }, [dispatch, shortcutClaims]);
  const grouped = useMemo(() => {
    const groups = new Map<string, MountedRemoteUi[]>();
    for (const mount of snapshot.mounts) {
      const point = mount.placement.point;
      const values = groups.get(point) ?? [];
      values.push(mount);
      groups.set(point, values);
    }
    return groups;
  }, [snapshot.mounts]);
  return (
    <div className="remote-ui-root" data-ui-client={capabilities.client}>
      {Object.entries(snapshot.errors).map(([id, message]) => <div className="ui-host-error" role="alert" key={id}><strong>Extension surface error</strong><span>{message}</span></div>)}
      {[...grouped].map(([point, mounts]) => <ContributionGroup key={point} point={point} mounts={mounts} dispatch={dispatch} showTerminalFallback={showTerminalFallback} />)}
    </div>
  );
}
