/**
 * The model writes Markdown. Showing it raw meant reading `##` and `**` as
 * literal characters, which is what the transcript did.
 *
 * The security property is the one to hold on to: this renders React elements
 * and never HTML, so model output arriving over a socket cannot inject into the
 * webview. The tests below check both halves — that Markdown becomes structure,
 * and that markup in the text stays text.
 */
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { renderMarkdown } from "../src/markdown.js";

function draw(text: string) {
  return render(<div data-testid="md">{renderMarkdown(text)}</div>);
}

describe("markdown rendering", () => {
  it("renders headings as structure, not as hashes", () => {
    draw("## Dead weight / hygiene");
    const root = screen.getByTestId("md");
    expect(root.textContent).toBe("Dead weight / hygiene");
    expect(root.textContent).not.toContain("##");
  });

  it("renders bold and inline code without their delimiters", () => {
    draw("**`main.py`** is a stub");
    const root = screen.getByTestId("md");
    expect(root.querySelector("strong")).toBeTruthy();
    expect(root.textContent).toBe("**`main.py`** is a stub".replace(/\*\*/g, "").replace(/`/g, ""));
  });

  it("renders a fenced code block verbatim, delimiters removed", () => {
    draw("before\n```rust\nlet x = 1;\n```\nafter");
    const root = screen.getByTestId("md");
    const pre = root.querySelector("pre");
    expect(pre?.textContent).toBe("let x = 1;");
    expect(root.textContent).not.toContain("```");
  });

  it("renders a bullet list as list items", () => {
    draw("- one\n- two\n- three");
    const items = screen.getByTestId("md").querySelectorAll("li");
    expect(items).toHaveLength(3);
    expect(items[1].textContent).toBe("two");
  });

  it("renders an ordered list as an ordered list", () => {
    const view = draw("1. first\n2. second");
    expect(view.container.querySelector("ol")).toBeTruthy();
    expect(view.container.querySelectorAll("li")).toHaveLength(2);
  });

  it("NEVER emits HTML from the model's text", () => {
    draw('<img src=x onerror="alert(1)"> and <b>bold</b>');
    const root = screen.getByTestId("md");
    // The markup survives as literal text...
    expect(root.textContent).toContain('<img src=x onerror="alert(1)">');
    // ...and produced no elements of its own.
    expect(root.querySelector("img")).toBeNull();
    expect(root.querySelector("b")).toBeNull();
  });

  it("leaves a link as text plus its target rather than a clickable anchor", () => {
    draw("see [the docs](https://example.com/x)");
    const root = screen.getByTestId("md");
    expect(root.querySelector("a")).toBeNull();
    expect(root.textContent).toContain("the docs (https://example.com/x)");
  });

  it("leaves unsupported syntax as the plain text it already was", () => {
    draw("a | table | row\n--- | --- | ---");
    expect(screen.getByTestId("md").textContent).toContain("a | table | row");
  });
});

describe("long system notes fold instead of walling the transcript", () => {
  it("folds the daemon's worktree-retention note behind a summary", async () => {
    const { Transcript } = await import("../src/components/Transcript.js");
    const note =
      "Kept the worktree /Users/x/codypendent-worktrees/s/run-abc and its branch `codypendent/run-abc`: " +
      "it held uncommitted changes, so nothing was deleted. Its diff is saved as artifact 01a0-e2a0.\n" +
      "Recover or discard it with `git worktree remove ...` and `git branch -D ...`.";
    render(
      <Transcript
        items={[{ id: "s1", type: "system", text: note, timestamp: "t" }]}
        connectionStatus="connected"
      />,
    );
    const details = document.querySelector("details");
    expect(details).toBeTruthy();
    // Folded by default, and the recovery text is kept rather than discarded.
    expect(details?.hasAttribute("open")).toBe(false);
    expect(details?.textContent).toContain("git branch -D");
  });

  it("leaves a short system line inline", async () => {
    const { Transcript } = await import("../src/components/Transcript.js");
    render(
      <Transcript
        items={[{ id: "s2", type: "system", text: "Run cancelled", timestamp: "t" }]}
        connectionStatus="connected"
      />,
    );
    expect(document.querySelector("details")).toBeNull();
    expect(screen.getByText("Run cancelled")).toBeTruthy();
  });
});
