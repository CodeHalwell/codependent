/**
 * The palette must reach every surface.
 *
 * The TUI is keyboard-driven and its palette is the front door to every
 * command. A desktop view that is only reachable by hunting the sidebar is
 * half-shipped, and a view added to the sidebar but forgotten in a
 * hand-written palette table is the unregistered-handler bug wearing a
 * different hat.
 *
 * `NAV_GROUPS` is now the single table both surfaces read, and
 * `completePaletteEntries` fills whatever the app did not curate. These tests
 * hold that end to end: the first two against the pure function, the last
 * through the real `App`, whose palette rows are the ones a user actually
 * sees.
 */
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App } from "../src/App.js";
import {
  completePaletteEntries,
  viewCommandId,
  type PaletteEntry,
} from "../src/components/CommandPalette.js";
import { NAV_GROUPS, type DesktopView } from "../src/components/Navigation.js";

/**
 * Every view the sidebar offers.
 *
 * `NAV_COVERS_EVERY_VIEW` in `Navigation.tsx` is the compile-time proof that
 * this is also every member of `DesktopView`; a runtime list cannot enumerate
 * a string union, so the two guards are complementary rather than redundant.
 */
const ALL_VIEWS: DesktopView[] = NAV_GROUPS.flatMap((navGroup) =>
  navGroup.views.map(([view]) => view),
);

describe("completePaletteEntries", () => {
  it("offers a command for every view, even when the app curates none", () => {
    const ids = new Set(completePaletteEntries([]).map((entry) => entry.id));
    const missing = ALL_VIEWS.filter((view) => !ids.has(viewCommandId(view)));
    expect(missing).toEqual([]);
  });

  it("keeps the app's own entry for a view rather than duplicating it", () => {
    const curated: PaletteEntry[] = [
      {
        id: viewCommandId("memory"),
        title: "/memory  Memory",
        description: "browse curated memories and their provenance",
        key: "M",
        group: "Workspace",
      },
      {
        id: "action:cancel",
        title: "Cancel run",
        description: "cancel the selected run",
        key: "—",
        group: "Run",
      },
    ];
    const completed = completePaletteEntries(curated);

    const memory = completed.filter((entry) => entry.id === viewCommandId("memory"));
    expect(memory).toHaveLength(1);
    expect(memory[0].title).toBe("/memory  Memory");
    expect(memory[0].key).toBe("M");

    // A non-view command is carried through untouched, and the curated rows
    // keep their order at the head of the table — the palette ranks ties by
    // table order, so this is behaviour, not cosmetics.
    expect(completed[0].id).toBe(viewCommandId("memory"));
    expect(completed[1].id).toBe("action:cancel");

    const ids = new Set(completed.map((entry) => entry.id));
    expect(ALL_VIEWS.filter((view) => !ids.has(viewCommandId(view)))).toEqual([]);
  });

  it("derives a title and description from the sidebar, never from nothing", () => {
    const derived = completePaletteEntries([]);
    for (const entry of derived) {
      expect(entry.title.trim().length).toBeGreaterThan(0);
      expect(entry.description.trim().length).toBeGreaterThan(0);
      expect(entry.group.trim().length).toBeGreaterThan(0);
    }
  });
});

describe("the palette the user actually gets", () => {
  it("lists a command for every view in NAV_GROUPS", () => {
    render(<App initialView="sessions" />);
    fireEvent.keyDown(window, { key: "k", metaKey: true });

    const dialog = screen.getByRole("dialog", { name: "Command palette" });
    const offered = new Set(
      Array.from(dialog.querySelectorAll("[data-command-id]")).map(
        (node) => node.getAttribute("data-command-id") ?? "",
      ),
    );

    const missing = ALL_VIEWS.filter((view) => !offered.has(viewCommandId(view)));
    expect(
      missing,
      "these views are in the sidebar but not in the command palette — the palette " +
        "is the front door to every surface, so a view missing here is half-shipped",
    ).toEqual([]);
  });

  it("navigates to a Configuration view chosen from the palette", () => {
    // The four Configuration views are the ones the app's curated table never
    // listed. Prove one of them is genuinely dispatchable from the palette,
    // not merely present as a row.
    render(<App initialView="sessions" />);
    fireEvent.keyDown(window, { key: "k", metaKey: true });

    const dialog = screen.getByRole("dialog", { name: "Command palette" });
    const row = dialog.querySelector(`[data-command-id="${viewCommandId("keys")}"]`);
    expect(row).not.toBeNull();
    fireEvent.click(row as Element);

    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
    expect(screen.getByRole("heading", { name: "API keys" })).toBeTruthy();
  });

  it("closes on Escape without navigating", () => {
    render(<App initialView="sessions" />);
    fireEvent.keyDown(window, { key: "k", metaKey: true });
    expect(screen.getByRole("dialog", { name: "Command palette" })).toBeTruthy();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
  });

  it("moves the selection with the arrow keys and runs the row Enter lands on", () => {
    render(<App initialView="sessions" />);
    fireEvent.keyDown(window, { key: "k", metaKey: true });

    const dialog = screen.getByRole("dialog", { name: "Command palette" });
    const filter = screen.getByLabelText("Command palette filter");
    const ids = Array.from(dialog.querySelectorAll("[data-command-id]")).map(
      (node) => node.getAttribute("data-command-id") ?? "",
    );
    const secondRow = ids[1];
    expect(secondRow.startsWith("view:")).toBe(true);

    fireEvent.keyDown(filter, { key: "ArrowDown" });
    fireEvent.keyDown(filter, { key: "Enter" });

    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
  });
});
