/**
 * The code graph could only be read as numbers and a paged table. Everything a
 * composition plot needs already arrives with `ReadCodeGraphStatus`, so this
 * draws what was already fetched rather than issuing a second query.
 */
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { CodeGraphStatusView } from "@codypendent/protocol";
import { GraphPlot } from "../src/components/GraphPlot.js";

const STATUS = {
  repository_root: "/repo",
  nodes: 1454,
  edges: 3210,
  files: 96,
  by_language: [
    { language: "python", files: 80, nodes: 1200, edges: 2800 },
    { language: "toml", files: 16, nodes: 254, edges: 410 },
  ],
  by_kind: [
    { label: "function", count: 900 },
    { label: "type", count: 300 },
    { label: "file", count: 254 },
  ],
  revisions: [{ label: "abc123", count: 1454 }],
  head_revision: "abc123",
  working_tree_dirty: false,
  stale: false,
} as unknown as CodeGraphStatusView;

describe("the code-graph plot", () => {
  it("shows the totals the status already carried", () => {
    render(<GraphPlot status={STATUS} />);
    expect(screen.getByText("1,454")).toBeTruthy();
    expect(screen.getByText("3,210")).toBeTruthy();
    expect(screen.getByText("96")).toBeTruthy();
  });

  it("draws a bar per language and per kind, with shares", () => {
    render(<GraphPlot status={STATUS} />);
    expect(screen.getByTestId("bar-python")).toBeTruthy();
    expect(screen.getByTestId("bar-toml")).toBeTruthy();
    expect(screen.getByTestId("bar-function")).toBeTruthy();
    // python is 1200 of 1454 language nodes.
    expect(screen.getByText(/1,200 · 83%/)).toBeTruthy();
  });

  it("scales bars against the largest row so a minority is still visible", () => {
    render(<GraphPlot status={STATUS} />);
    const minority = screen.getByTestId("bar-toml").getAttribute("style") ?? "";
    // 254/1200 ≈ 21%, not rounded away to nothing.
    expect(minority).toMatch(/width: 2[01]\./);
  });

  it("says so when the graph does not describe the working tree", () => {
    render(<GraphPlot status={{ ...STATUS, stale: true, stale_reason: "head moved" }} />);
    expect(screen.getByText(/does not describe the current working tree: head moved/)).toBeTruthy();
  });

  it("does not pretend to have data it was not given", () => {
    render(<GraphPlot status={{ ...STATUS, by_language: [], by_kind: [] }} />);
    expect(screen.getAllByText("nothing recorded")).toHaveLength(2);
  });
});
