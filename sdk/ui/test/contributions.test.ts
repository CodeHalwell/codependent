import { describe, expect, it } from "vitest";
import {
  ContributionRegistry,
  MINIMAL_TERMINAL_CAPABILITIES,
  Table,
  Text,
  TraceView,
  type UiCapabilities,
} from "../src/index.js";

describe("ContributionRegistry", () => {
  it("indexes by point and renderer and requires web terminal fallbacks", () => {
    const registry = new ContributionRegistry();
    registry.register({
      id: "acme.trace",
      point: "artifact-renderer",
      renderer: "application/vnd.acme.trace+json",
      target: "web",
      render: () => Text({ value: "graphical trace" }),
      terminalFallback: () => Text({ value: "text trace" }),
    });
    const definition = registry.get("acme.trace");
    expect(definition).toBeDefined();
    expect(registry.render(definition!, null, MINIMAL_TERMINAL_CAPABILITIES)).toMatchObject({ type: "Text", props: { value: "text trace" } });
  });

  it("renders rich web trace data through its declared terminal TraceTable surface", () => {
    const registry = new ContributionRegistry();
    registry.register({
      id: "acme.trace-table",
      point: "trace-span-renderer",
      renderer: "acme.TraceTable",
      target: "terminal",
      render: ({ data }) => Table({
        columns: [{ key: "name", label: "Span" }, { key: "duration", label: "Duration" }],
        rows: Array.isArray(data) ? data : [],
        accessibleLabel: "Trace spans table",
      }),
    });
    registry.register({
      id: "acme.trace-web",
      point: "trace-span-renderer",
      renderer: "acme.Trace",
      target: "web",
      render: ({ data }) => TraceView({ data, accessibleLabel: "Interactive trace" }),
      terminalFallback: { rendererId: "acme.trace-table" },
    });
    const data = [{ name: "root", duration: 12 }, { name: "tool", duration: 7 }];
    const web = {
      ...MINIMAL_TERMINAL_CAPABILITIES,
      client: "web",
      primitives: "*",
    } satisfies UiCapabilities;
    const definition = registry.get("acme.trace-web");
    expect(registry.render(definition!, data, web)).toMatchObject({
      type: "TraceView",
      props: { data, accessibleLabel: "Interactive trace" },
    });
    expect(registry.render(definition!, data, MINIMAL_TERMINAL_CAPABILITIES)).toMatchObject({
      type: "Table",
      props: { rows: data, accessibleLabel: "Trace spans table" },
    });
  });
});
