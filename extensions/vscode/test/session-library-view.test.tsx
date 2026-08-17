// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  mergeSessionSearchPage,
  SessionLibraryView,
  type SessionLibraryItem,
} from "../src/webview/session-library-view.js";

declare global {
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean;
}

const active: SessionLibraryItem = {
  stableIdentity: "session:one:title",
  sessionId: "11111111-1111-4111-8111-111111111111",
  title: "Parser investigation",
  state: "open",
  updatedAt: "2026-08-17T12:00:00Z",
  pinned: true,
  archived: false,
  source: "Title",
  excerpt: "Parser investigation",
};

const archived: SessionLibraryItem = {
  ...active,
  stableIdentity: "session:two:transcript:4",
  sessionId: "22222222-2222-4222-8222-222222222222",
  title: "Old compiler work",
  pinned: false,
  archived: true,
  source: "Transcript",
  excerpt: "The parser failed on unicode input",
};

describe("SessionLibraryView", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("renders searchable session results and exposes deep-link and lifecycle actions", () => {
    const onSearch = vi.fn();
    const onOpen = vi.fn();
    const onMutate = vi.fn();
    act(() => root.render(
      <SessionLibraryView
        status="ready"
        items={[active, archived]}
        nextCursor="next-page"
        onSearch={onSearch}
        onOpen={onOpen}
        onLoadMore={vi.fn()}
        onMutate={onMutate}
      />,
    ));

    const search = container.querySelector<HTMLInputElement>('input[type="search"]');
    expect(search?.getAttribute("aria-label")).toBe("Search sessions");
    act(() => {
      if (search) search.value = "unicode";
      search?.dispatchEvent(new Event("input", { bubbles: true }));
      search?.form?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
    expect(onSearch).toHaveBeenCalledWith("unicode");

    const cards = container.querySelectorAll<HTMLElement>("[data-session-id]");
    expect(cards).toHaveLength(2);
    expect(cards[0].textContent).toContain("Pinned");
    expect(cards[1].textContent).toContain("Transcript");
    expect(cards[1].textContent).toContain("unicode input");

    act(() => cards[1].querySelector<HTMLButtonElement>('[data-action="open"]')?.click());
    expect(onOpen).toHaveBeenCalledWith(archived.sessionId);
    act(() => cards[0].querySelector<HTMLButtonElement>('[data-action="archive"]')?.click());
    act(() => cards[1].querySelector<HTMLButtonElement>('[data-action="restore"]')?.click());
    expect(onMutate).toHaveBeenNthCalledWith(1, active.sessionId, { type: "Archive" });
    expect(onMutate).toHaveBeenNthCalledWith(2, archived.sessionId, { type: "Restore" });
  });

  it("supports rename, pin toggling, paging, and honest loading/error/empty states", () => {
    const onMutate = vi.fn();
    const onLoadMore = vi.fn();
    const { rerender } = (() => {
      const render = (status: "loading" | "ready" | "error", items: SessionLibraryItem[], error?: string) => act(() => root.render(
        <SessionLibraryView status={status} items={items} nextCursor="cursor" error={error}
          onSearch={vi.fn()} onOpen={vi.fn()} onLoadMore={onLoadMore} onMutate={onMutate} />,
      ));
      render("ready", [active]);
      return { rerender: render };
    })();

    const card = container.querySelector<HTMLElement>("[data-session-id]");
    act(() => card?.querySelector<HTMLButtonElement>('[data-action="rename"]')?.click());
    const rename = card?.querySelector<HTMLInputElement>('input[aria-label="New session title"]');
    act(() => {
      if (rename) rename.value = "Unicode parser fix";
      rename?.dispatchEvent(new Event("input", { bubbles: true }));
      rename?.form?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
    expect(onMutate).toHaveBeenCalledWith(active.sessionId, { type: "Rename", title: "Unicode parser fix" });

    act(() => card?.querySelector<HTMLButtonElement>('[data-action="unpin"]')?.click());
    expect(onMutate).toHaveBeenCalledWith(active.sessionId, { type: "Unpin" });
    act(() => container.querySelector<HTMLButtonElement>('[data-action="load-more"]')?.click());
    expect(onLoadMore).toHaveBeenCalledWith("cursor");

    rerender("loading", []);
    expect(container.querySelector('[role="status"]')?.textContent).toContain("Loading sessions");
    rerender("error", [], "Daemon unavailable");
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("Daemon unavailable");
    rerender("ready", []);
    expect(container.textContent).toContain("No sessions found");
  });
});

describe("mergeSessionSearchPage", () => {
  it("deduplicates overlapping pages by stable result identity", () => {
    expect(mergeSessionSearchPage([active], [active, archived])).toEqual([active, archived]);
  });
});
