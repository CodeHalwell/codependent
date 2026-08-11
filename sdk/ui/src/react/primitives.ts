import { createElement, type ComponentType, type ReactNode } from "react";
import type { UiEvent, UiNode, UiPrimitive } from "../protocol.js";
import type { PrimitivePropsMap } from "../primitives.js";

export type ReactUiEventHandler = (event: UiEvent) => void;

export interface ReactEventProps {
  /** Worker-local press handler. Presence is serialized; the function never crosses the boundary. */
  onPress?: ReactUiEventHandler;
  onAction?: ReactUiEventHandler;
  onChange?: ReactUiEventHandler;
  onSubmit?: ReactUiEventHandler;
  onSelect?: ReactUiEventHandler;
  onNavigate?: ReactUiEventHandler;
  onFocus?: ReactUiEventHandler;
  onBlur?: ReactUiEventHandler;
  onCustom?: ReactUiEventHandler;
}

export type ReactPrimitiveProps<K extends UiPrimitive> = Omit<PrimitivePropsMap[K], "children" | "fallback"> & ReactEventProps & {
  children?: ReactNode;
  /** Fallbacks are already semantic nodes, so they remain renderer-independent. */
  fallback?: UiNode;
};

export type ReactPrimitiveComponent<K extends UiPrimitive> = ComponentType<ReactPrimitiveProps<K>>;

export function createReactPrimitive<K extends UiPrimitive>(type: K): ReactPrimitiveComponent<K> {
  function CodypendentPrimitive(props: ReactPrimitiveProps<K>): ReactNode {
    return createElement(type, props);
  }
  CodypendentPrimitive.displayName = `Codypendent.${type}`;
  return CodypendentPrimitive;
}

export const Box = createReactPrimitive("Box");
export const Stack = createReactPrimitive("Stack");
export const Row = createReactPrimitive("Row");
export const Grid = createReactPrimitive("Grid");
export const Split = createReactPrimitive("Split");
export const Spacer = createReactPrimitive("Spacer");
export const ScrollArea = createReactPrimitive("ScrollArea");
export const VirtualList = createReactPrimitive("VirtualList");
export const Text = createReactPrimitive("Text");
export const Markdown = createReactPrimitive("Markdown");
export const Code = createReactPrimitive("Code");
export const Diff = createReactPrimitive("Diff");
export const Image = createReactPrimitive("Image");
export const Audio = createReactPrimitive("Audio");
export const JsonTree = createReactPrimitive("JsonTree");
export const LogViewer = createReactPrimitive("LogViewer");
export const List = createReactPrimitive("List");
export const Table = createReactPrimitive("Table");
export const Tree = createReactPrimitive("Tree");
export const KeyValue = createReactPrimitive("KeyValue");
export const Timeline = createReactPrimitive("Timeline");
export const Graph = createReactPrimitive("Graph");
export const Chart = createReactPrimitive("Chart");
export const Sparkline = createReactPrimitive("Sparkline");
export const Badge = createReactPrimitive("Badge");
export const Progress = createReactPrimitive("Progress");
export const Spinner = createReactPrimitive("Spinner");
export const Alert = createReactPrimitive("Alert");
export const Toast = createReactPrimitive("Toast");
export const EmptyState = createReactPrimitive("EmptyState");
export const ErrorBoundary = createReactPrimitive("ErrorBoundary");
export const Tabs = createReactPrimitive("Tabs");
export const Breadcrumb = createReactPrimitive("Breadcrumb");
export const Menu = createReactPrimitive("Menu");
export const CommandList = createReactPrimitive("CommandList");
export const Pagination = createReactPrimitive("Pagination");
export const Link = createReactPrimitive("Link");
export const Details = createReactPrimitive("Details");
export const TextInput = createReactPrimitive("TextInput");
export const TextArea = createReactPrimitive("TextArea");
export const Select = createReactPrimitive("Select");
export const MultiSelect = createReactPrimitive("MultiSelect");
export const Checkbox = createReactPrimitive("Checkbox");
export const Radio = createReactPrimitive("Radio");
export const Form = createReactPrimitive("Form");
export const Button = createReactPrimitive("Button");
export const ActionMenu = createReactPrimitive("ActionMenu");
export const Toolbar = createReactPrimitive("Toolbar");
export const ContextMenu = createReactPrimitive("ContextMenu");
export const ToolCard = createReactPrimitive("ToolCard");
export const ArtifactCard = createReactPrimitive("ArtifactCard");
export const ApprovalCard = createReactPrimitive("ApprovalCard");
export const AgentCard = createReactPrimitive("AgentCard");
export const WorkflowNode = createReactPrimitive("WorkflowNode");
export const PatchCard = createReactPrimitive("PatchCard");
export const TestReport = createReactPrimitive("TestReport");
export const PermissionDiff = createReactPrimitive("PermissionDiff");
export const TraceView = createReactPrimitive("TraceView");
export const CostView = createReactPrimitive("CostView");
export const TerminalOnly = createReactPrimitive("TerminalOnly");
export const WebOnly = createReactPrimitive("WebOnly");

/** Composition-first panel API; variants remain semantic and host-themed. */
const PanelRoot = (props: ReactPrimitiveProps<"Box">): ReactNode => createElement(Box, { border: "rounded", ...props });
const PanelHeader = (props: ReactPrimitiveProps<"Row">): ReactNode => createElement(Row, { align: "spaceBetween", ...props });
const PanelBody = (props: ReactPrimitiveProps<"Stack">): ReactNode => createElement(Stack, props);
const PanelFooter = (props: ReactPrimitiveProps<"Row">): ReactNode => createElement(Row, { align: "end", ...props });

export const Panel = {
  Root: PanelRoot,
  Header: PanelHeader,
  Body: PanelBody,
  Footer: PanelFooter,
} as const;
