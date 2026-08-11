import type { UiNode } from "./protocol.js";
import {
  createPrimitive,
  normalizeChildren,
  type PrimitiveProps,
  type PrimitivePropsMap,
  type UiChild,
} from "./primitives.js";

export const Fragment = Symbol.for("codypendent.ui.fragment");

type PureFunctionComponent<P = Record<string, unknown>> = (props: P) => UiChild;
type JsxType = keyof JSX.IntrinsicElements | PureFunctionComponent<never> | typeof Fragment;

const intrinsicTypes = {
  box: "Box", stack: "Stack", row: "Row", grid: "Grid", split: "Split", spacer: "Spacer",
  scrollArea: "ScrollArea", virtualList: "VirtualList", text: "Text", markdown: "Markdown",
  code: "Code", diff: "Diff", image: "Image", audio: "Audio", jsonTree: "JsonTree",
  logViewer: "LogViewer", list: "List", table: "Table", tree: "Tree", keyValue: "KeyValue",
  timeline: "Timeline", graph: "Graph", chart: "Chart", sparkline: "Sparkline", badge: "Badge",
  progress: "Progress", spinner: "Spinner", alert: "Alert", toast: "Toast", emptyState: "EmptyState",
  errorBoundary: "ErrorBoundary", tabs: "Tabs", breadcrumb: "Breadcrumb", menu: "Menu",
  commandList: "CommandList", pagination: "Pagination", link: "Link", details: "Details",
  textInput: "TextInput", textArea: "TextArea", select: "Select", multiSelect: "MultiSelect",
  checkbox: "Checkbox", radio: "Radio", form: "Form", button: "Button", actionMenu: "ActionMenu",
  toolbar: "Toolbar", contextMenu: "ContextMenu", toolCard: "ToolCard", artifactCard: "ArtifactCard",
  approvalCard: "ApprovalCard", agentCard: "AgentCard", workflowNode: "WorkflowNode", patchCard: "PatchCard",
  testReport: "TestReport", permissionDiff: "PermissionDiff", traceView: "TraceView", costView: "CostView",
  terminalOnly: "TerminalOnly", webOnly: "WebOnly",
} as const;

function withKey(child: UiNode, key: string | number | undefined): UiNode {
  if (key === undefined || child.id !== undefined) return child;
  return { ...child, id: String(key) };
}

export function jsx<P>(type: PureFunctionComponent<P>, props: P, key?: string | number): UiNode;
export function jsx<K extends keyof JSX.IntrinsicElements>(type: K, props: JSX.IntrinsicElements[K], key?: string | number): UiNode;
export function jsx(type: typeof Fragment, props: { children?: UiChild }, key?: string | number): UiNode;
export function jsx(type: JsxType, props: Record<string, unknown> | null, key?: string | number): UiNode {
  const normalizedProps = props ?? {};
  if (type === Fragment) {
    return withKey(createPrimitive("Stack")({ children: normalizedProps.children as UiChild }), key);
  }
  if (typeof type === "function") {
    const children = normalizeChildren(type(normalizedProps as never));
    if (children.length !== 1) return withKey(createPrimitive("Stack")({ children }), key);
    return withKey(children[0] as UiNode, key);
  }
  const semanticType = intrinsicTypes[type];
  return withKey(createPrimitive(semanticType)(normalizedProps as never), key);
}

export const jsxs = jsx;
export const jsxDEV = jsx;

export namespace JSX {
  export type Element = UiNode;
  export interface ElementChildrenAttribute { children: unknown; }
  export interface IntrinsicAttributes { key?: string | number; }
  export interface IntrinsicElements {
    box: PrimitivePropsMap["Box"];
    stack: PrimitivePropsMap["Stack"];
    row: PrimitivePropsMap["Row"];
    grid: PrimitivePropsMap["Grid"];
    split: PrimitivePropsMap["Split"];
    spacer: PrimitivePropsMap["Spacer"];
    scrollArea: PrimitivePropsMap["ScrollArea"];
    virtualList: PrimitivePropsMap["VirtualList"];
    text: PrimitivePropsMap["Text"];
    markdown: PrimitivePropsMap["Markdown"];
    code: PrimitivePropsMap["Code"];
    diff: PrimitivePropsMap["Diff"];
    image: PrimitivePropsMap["Image"];
    audio: PrimitivePropsMap["Audio"];
    jsonTree: PrimitivePropsMap["JsonTree"];
    logViewer: PrimitivePropsMap["LogViewer"];
    list: PrimitivePropsMap["List"];
    table: PrimitivePropsMap["Table"];
    tree: PrimitivePropsMap["Tree"];
    keyValue: PrimitivePropsMap["KeyValue"];
    timeline: PrimitivePropsMap["Timeline"];
    graph: PrimitivePropsMap["Graph"];
    chart: PrimitivePropsMap["Chart"];
    sparkline: PrimitivePropsMap["Sparkline"];
    badge: PrimitivePropsMap["Badge"];
    progress: PrimitivePropsMap["Progress"];
    spinner: PrimitivePropsMap["Spinner"];
    alert: PrimitivePropsMap["Alert"];
    toast: PrimitivePropsMap["Toast"];
    emptyState: PrimitivePropsMap["EmptyState"];
    errorBoundary: PrimitivePropsMap["ErrorBoundary"];
    tabs: PrimitivePropsMap["Tabs"];
    breadcrumb: PrimitivePropsMap["Breadcrumb"];
    menu: PrimitivePropsMap["Menu"];
    commandList: PrimitivePropsMap["CommandList"];
    pagination: PrimitivePropsMap["Pagination"];
    link: PrimitivePropsMap["Link"];
    details: PrimitivePropsMap["Details"];
    textInput: PrimitivePropsMap["TextInput"];
    textArea: PrimitivePropsMap["TextArea"];
    select: PrimitivePropsMap["Select"];
    multiSelect: PrimitivePropsMap["MultiSelect"];
    checkbox: PrimitivePropsMap["Checkbox"];
    radio: PrimitivePropsMap["Radio"];
    form: PrimitivePropsMap["Form"];
    button: PrimitivePropsMap["Button"];
    actionMenu: PrimitivePropsMap["ActionMenu"];
    toolbar: PrimitivePropsMap["Toolbar"];
    contextMenu: PrimitivePropsMap["ContextMenu"];
    toolCard: PrimitivePropsMap["ToolCard"];
    artifactCard: PrimitivePropsMap["ArtifactCard"];
    approvalCard: PrimitivePropsMap["ApprovalCard"];
    agentCard: PrimitivePropsMap["AgentCard"];
    workflowNode: PrimitivePropsMap["WorkflowNode"];
    patchCard: PrimitivePropsMap["PatchCard"];
    testReport: PrimitivePropsMap["TestReport"];
    permissionDiff: PrimitivePropsMap["PermissionDiff"];
    traceView: PrimitivePropsMap["TraceView"];
    costView: PrimitivePropsMap["CostView"];
    terminalOnly: PrimitivePropsMap["TerminalOnly"];
    webOnly: PrimitivePropsMap["WebOnly"];
  }
}

export type { PrimitiveProps };
