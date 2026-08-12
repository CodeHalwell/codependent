import type {
  UiElementNode,
  UiJsonValue,
  UiNode,
  UiPrimitive,
  UiRequirement,
} from "./protocol.js";

export type UiChild = UiNode | string | number | boolean | null | undefined | readonly UiChild[];
export type Tone = "neutral" | "muted" | "positive" | "warning" | "critical" | "info";
export type Size = "xs" | "sm" | "md" | "lg" | "xl";
export type Align = "start" | "center" | "end" | "stretch" | "spaceBetween" | "spaceAround";
/**
 * A `Grid` track template: an equal-track count (`3` → three `1fr` tracks) or
 * an explicit track list (`"2fr"`, `"40%"`, `"auto"`, or a plain cell count).
 * Both hosts normalize it the same way; counts above 24 are clamped.
 */
export type GridTracks = number | readonly (number | string)[];

export interface PrimitiveProps {
  id?: string;
  children?: UiChild;
  fallback?: UiChild;
  requires?: UiRequirement | readonly UiRequirement[];
  accessibleLabel?: string;
  description?: string;
  testId?: string;
  hidden?: boolean;
  /** Worker-local semantic events for pure (non-React) surfaces. */
  localEvents?: readonly import("./protocol.js").UiEventType[];
}

export interface LayoutProps extends PrimitiveProps {
  gap?: Size | number;
  padding?: Size | number;
  align?: Align;
  width?: number | string;
  height?: number | string;
  grow?: number;
  wrap?: boolean;
  border?: boolean | "single" | "double" | "rounded";
  title?: string;
  /** `Grid` only — see {@link GridTracks}. Ignored by every other layout primitive. */
  columns?: GridTracks;
}

export interface TextProps extends PrimitiveProps {
  value?: string;
  role?: "text" | "heading" | "label" | "caption" | "status";
  tone?: Tone;
  weight?: "normal" | "medium" | "bold";
  italic?: boolean;
  underline?: boolean;
  truncate?: boolean;
}

export interface CodeProps extends PrimitiveProps {
  value?: string;
  language?: string;
  lineNumbers?: boolean;
  startLine?: number;
  wrap?: boolean;
}

export interface MarkdownProps extends PrimitiveProps { source: string; }
export interface DiffProps extends PrimitiveProps {
  before?: string;
  after?: string;
  patch?: string;
  path?: string;
  mode?: "unified" | "sideBySide";
}
export interface MediaProps extends PrimitiveProps {
  src: string;
  alt: string;
  caption?: string;
  transcript?: string;
}
export interface CollectionProps extends PrimitiveProps {
  items?: readonly UiJsonValue[];
  selectedKey?: string;
  emptyMessage?: string;
  virtualized?: boolean;
}
export interface TableProps extends CollectionProps {
  columns: readonly ({ key: string; label: string; width?: number | string; align?: "left" | "center" | "right" } | string)[];
  rows: readonly UiJsonValue[];
  sortKey?: string;
  sortDirection?: "ascending" | "descending";
}
export interface TreeProps extends CollectionProps { expandedKeys?: readonly string[]; }
export interface GraphProps extends PrimitiveProps {
  nodes: readonly UiJsonValue[];
  edges: readonly UiJsonValue[];
  direction?: "horizontal" | "vertical";
}
export interface ChartProps extends PrimitiveProps {
  data: readonly UiJsonValue[];
  chartType?: "line" | "bar" | "area" | "scatter";
  xKey?: string;
  yKey?: string;
}
export interface FeedbackProps extends PrimitiveProps {
  tone?: Tone;
  title?: string;
  message?: string;
  dismissAction?: string;
}
export interface ProgressProps extends PrimitiveProps {
  value?: number;
  maximum?: number;
  indeterminate?: boolean;
  label?: string;
}
export interface TabsProps extends PrimitiveProps {
  tabs: readonly { id: string; label: string; disabled?: boolean }[];
  activeId: string;
  changeAction?: string;
}
export interface NavigationProps extends PrimitiveProps {
  items?: readonly UiJsonValue[];
  action?: string;
  href?: string;
  current?: string | number;
  total?: number;
}
export interface InputProps extends PrimitiveProps {
  name: string;
  /** Non-secret browser input hint. Passwords and credentials are host-owned. */
  inputType?: "text" | "email" | "url" | "search" | "number" | "tel";
  value?: UiJsonValue;
  defaultValue?: UiJsonValue;
  placeholder?: string;
  required?: boolean;
  disabled?: boolean;
  readOnly?: boolean;
  changeAction?: string;
  validateAction?: string;
}
export interface ChoiceProps extends InputProps {
  options?: readonly { value: string; label: string; disabled?: boolean }[];
  checked?: boolean;
}
export interface ButtonProps extends PrimitiveProps {
  /** Host-mediated action id. Omit for a worker-local React `onPress` handler. */
  action?: string;
  label?: string;
  tone?: Tone;
  disabled?: boolean;
  shortcut?: string;
  payload?: UiJsonValue;
}
export interface FormProps extends PrimitiveProps {
  /** Host-mediated submit command. Omit for a worker-local React onSubmit. */
  submitAction?: string;
  resetAction?: string;
}
export interface DomainCardProps extends LayoutProps {
  resourceId?: string;
  status?: string;
  actions?: readonly string[];
  data?: UiJsonValue;
}

export interface PrimitivePropsMap {
  Box: LayoutProps; Stack: LayoutProps; Row: LayoutProps; Grid: LayoutProps;
  Split: LayoutProps & { ratio?: number; direction?: "horizontal" | "vertical" };
  Spacer: PrimitiveProps & { size?: Size | number };
  ScrollArea: LayoutProps & { axis?: "horizontal" | "vertical" | "both" };
  VirtualList: CollectionProps;
  Text: TextProps; Markdown: MarkdownProps; Code: CodeProps; Diff: DiffProps;
  Image: MediaProps; Audio: MediaProps; JsonTree: PrimitiveProps & { value: UiJsonValue; expandedDepth?: number };
  LogViewer: PrimitiveProps & { lines: readonly UiJsonValue[]; level?: string; filter?: string };
  List: CollectionProps; Table: TableProps; Tree: TreeProps;
  KeyValue: PrimitiveProps & { entries: Readonly<Record<string, UiJsonValue>> };
  Timeline: CollectionProps; Graph: GraphProps; Chart: ChartProps;
  Sparkline: PrimitiveProps & { values: readonly number[]; tone?: Tone };
  Badge: FeedbackProps; Progress: ProgressProps; Spinner: ProgressProps; Alert: FeedbackProps;
  Toast: FeedbackProps; EmptyState: FeedbackProps; ErrorBoundary: FeedbackProps;
  Tabs: TabsProps; Breadcrumb: NavigationProps; Menu: NavigationProps; CommandList: CollectionProps;
  Pagination: NavigationProps; Link: NavigationProps & { label?: string }; Details: LayoutProps & { open?: boolean };
  TextInput: InputProps; TextArea: InputProps & { rows?: number }; Select: ChoiceProps;
  MultiSelect: ChoiceProps; Checkbox: ChoiceProps; Radio: ChoiceProps; Form: FormProps;
  Button: ButtonProps; ActionMenu: NavigationProps; Toolbar: NavigationProps; ContextMenu: NavigationProps;
  ToolCard: DomainCardProps; ArtifactCard: DomainCardProps; ApprovalCard: DomainCardProps;
  AgentCard: DomainCardProps; WorkflowNode: DomainCardProps; PatchCard: DomainCardProps;
  TestReport: DomainCardProps; PermissionDiff: DomainCardProps; TraceView: DomainCardProps; CostView: DomainCardProps;
  TerminalOnly: PrimitiveProps; WebOnly: PrimitiveProps;
}

export type UiElementOf<K extends UiPrimitive> = UiElementNode<Readonly<Record<string, UiJsonValue | undefined>>> & { type: K };
export type PrimitiveComponent<K extends UiPrimitive> = (props: PrimitivePropsMap[K]) => UiElementOf<K>;

export function normalizeChildren(input: UiChild): UiNode[] {
  const output: UiNode[] = [];
  const visit = (child: UiChild): void => {
    if (Array.isArray(child)) {
      child.forEach(visit);
    } else if (child === null || child === undefined || typeof child === "boolean") {
      return;
    } else if (typeof child === "string" || typeof child === "number") {
      output.push({ kind: "text", text: String(child) });
    } else {
      output.push(child as UiNode);
    }
  };
  visit(input);
  return output;
}

export function createPrimitive<K extends UiPrimitive>(type: K): PrimitiveComponent<K> {
  return ((input: PrimitivePropsMap[K]) => {
    const { id, children, fallback, requires, localEvents, ...serializableProps } = input;
    const props = {
      ...serializableProps,
      ...(localEvents === undefined ? {} : { eventHandlers: [...localEvents] }),
    };
    const node: UiElementOf<K> = {
      kind: "element",
      type,
      props: props as Readonly<Record<string, UiJsonValue | undefined>>,
      children: normalizeChildren(children),
    };
    if (id !== undefined) node.id = id;
    const fallbackNodes = normalizeChildren(fallback);
    if (fallbackNodes.length === 1) node.fallback = fallbackNodes[0];
    if (fallbackNodes.length > 1) node.fallback = createPrimitive("Stack")({ children: fallbackNodes });
    if (requires !== undefined) node.requires = Array.isArray(requires) ? [...requires] : [requires as UiRequirement];
    return node;
  }) as PrimitiveComponent<K>;
}

export const Box = createPrimitive("Box");
export const Stack = createPrimitive("Stack");
export const Row = createPrimitive("Row");
export const Grid = createPrimitive("Grid");
export const Split = createPrimitive("Split");
export const Spacer = createPrimitive("Spacer");
export const ScrollArea = createPrimitive("ScrollArea");
export const VirtualList = createPrimitive("VirtualList");
export const Text = createPrimitive("Text");
export const Markdown = createPrimitive("Markdown");
export const Code = createPrimitive("Code");
export const Diff = createPrimitive("Diff");
export const Image = createPrimitive("Image");
export const Audio = createPrimitive("Audio");
export const JsonTree = createPrimitive("JsonTree");
export const LogViewer = createPrimitive("LogViewer");
export const List = createPrimitive("List");
export const Table = createPrimitive("Table");
export const Tree = createPrimitive("Tree");
export const KeyValue = createPrimitive("KeyValue");
export const Timeline = createPrimitive("Timeline");
export const Graph = createPrimitive("Graph");
export const Chart = createPrimitive("Chart");
export const Sparkline = createPrimitive("Sparkline");
export const Badge = createPrimitive("Badge");
export const Progress = createPrimitive("Progress");
export const Spinner = createPrimitive("Spinner");
export const Alert = createPrimitive("Alert");
export const Toast = createPrimitive("Toast");
export const EmptyState = createPrimitive("EmptyState");
export const ErrorBoundary = createPrimitive("ErrorBoundary");
export const Tabs = createPrimitive("Tabs");
export const Breadcrumb = createPrimitive("Breadcrumb");
export const Menu = createPrimitive("Menu");
export const CommandList = createPrimitive("CommandList");
export const Pagination = createPrimitive("Pagination");
export const Link = createPrimitive("Link");
export const Details = createPrimitive("Details");
export const TextInput = createPrimitive("TextInput");
export const TextArea = createPrimitive("TextArea");
export const Select = createPrimitive("Select");
export const MultiSelect = createPrimitive("MultiSelect");
export const Checkbox = createPrimitive("Checkbox");
export const Radio = createPrimitive("Radio");
export const Form = createPrimitive("Form");
export const Button = createPrimitive("Button");
export const ActionMenu = createPrimitive("ActionMenu");
export const Toolbar = createPrimitive("Toolbar");
export const ContextMenu = createPrimitive("ContextMenu");
export const ToolCard = createPrimitive("ToolCard");
export const ArtifactCard = createPrimitive("ArtifactCard");
export const ApprovalCard = createPrimitive("ApprovalCard");
export const AgentCard = createPrimitive("AgentCard");
export const WorkflowNode = createPrimitive("WorkflowNode");
export const PatchCard = createPrimitive("PatchCard");
export const TestReport = createPrimitive("TestReport");
export const PermissionDiff = createPrimitive("PermissionDiff");
export const TraceView = createPrimitive("TraceView");
export const CostView = createPrimitive("CostView");
export const TerminalOnly = createPrimitive("TerminalOnly");
export const WebOnly = createPrimitive("WebOnly");
