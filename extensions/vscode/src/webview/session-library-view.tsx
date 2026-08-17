import { useState, type FormEvent } from "react";

export interface SessionLibraryItem {
  stableIdentity: string;
  sessionId: string;
  title: string;
  state: string;
  updatedAt: string;
  pinned: boolean;
  archived: boolean;
  source: string;
  excerpt?: string;
}

export type SessionLifecycleAction =
  | { type: "Rename"; title: string }
  | { type: "Pin" }
  | { type: "Unpin" }
  | { type: "Archive" }
  | { type: "Restore" };

export interface SessionLibraryViewProps {
  status: "loading" | "ready" | "error";
  items: readonly SessionLibraryItem[];
  nextCursor?: string;
  error?: string;
  onSearch(query: string): void;
  onOpen(sessionId: string): void;
  onLoadMore(cursor: string): void;
  onMutate(sessionId: string, action: SessionLifecycleAction): void;
}

/** Merge cursor pages without duplicating the catch-up overlap. */
export function mergeSessionSearchPage(
  current: readonly SessionLibraryItem[],
  incoming: readonly SessionLibraryItem[],
): SessionLibraryItem[] {
  const merged = new Map(current.map((item) => [item.stableIdentity, item]));
  for (const item of incoming) merged.set(item.stableIdentity, item);
  return [...merged.values()];
}

/**
 * Host-neutral Session Library projection for the VS Code webview.
 * All callbacks are intents: daemon results remain the source of truth.
 */
export function SessionLibraryView({
  status,
  items,
  nextCursor,
  error,
  onSearch,
  onOpen,
  onLoadMore,
  onMutate,
}: SessionLibraryViewProps) {
  const [query, setQuery] = useState("");
  const [renaming, setRenaming] = useState<string>();
  const [title, setTitle] = useState("");

  function submitSearch(event: FormEvent): void {
    event.preventDefault();
    const value = new FormData(event.currentTarget as HTMLFormElement).get("query");
    onSearch(typeof value === "string" ? value.trim() : "");
  }

  function beginRename(item: SessionLibraryItem): void {
    setRenaming(item.sessionId);
    setTitle(item.title);
  }

  function submitRename(event: FormEvent, item: SessionLibraryItem): void {
    event.preventDefault();
    const value = new FormData(event.currentTarget as HTMLFormElement).get("title");
    const nextTitle = typeof value === "string" ? value.trim() : "";
    if (nextTitle.length === 0 || nextTitle === item.title) return;
    onMutate(item.sessionId, { type: "Rename", title: nextTitle });
    setRenaming(undefined);
  }

  return (
    <section aria-label="Session library" className="session-library">
      <form role="search" onSubmit={submitSearch}>
        <input
          type="search"
          name="query"
          aria-label="Search sessions"
          placeholder="Search sessions, transcripts, paths…"
          value={query}
          onChange={(event) => setQuery(event.currentTarget.value)}
        />
        <button type="submit">Search</button>
      </form>

      {status === "loading" && <p role="status">Loading sessions…</p>}
      {status === "error" && <p role="alert">{error ?? "Could not load sessions."}</p>}
      {status === "ready" && items.length === 0 && <p>No sessions found.</p>}

      {status === "ready" && items.length > 0 && (
        <ol aria-label="Session results" className="session-library-results">
          {items.map((item) => (
            <li key={item.stableIdentity} data-session-id={item.sessionId} className="session-library-card">
              <header>
                <button type="button" data-action="open" onClick={() => onOpen(item.sessionId)}>
                  {item.title}
                </button>
                {item.pinned && <span title="Pinned session">Pinned</span>}
                {item.archived && <span>Archived</span>}
              </header>
              <div className="session-library-metadata">
                <span>{item.source}</span>
                <span>{item.state}</span>
                <time dateTime={item.updatedAt}>{formatUpdatedAt(item.updatedAt)}</time>
              </div>
              {item.excerpt && <p>{item.excerpt}</p>}

              {renaming === item.sessionId ? (
                <form onSubmit={(event) => submitRename(event, item)}>
                  <input
                    autoFocus
                    name="title"
                    aria-label="New session title"
                    value={title}
                    onChange={(event) => setTitle(event.currentTarget.value)}
                  />
                  <button type="submit">Save</button>
                  <button type="button" onClick={() => setRenaming(undefined)}>Cancel</button>
                </form>
              ) : (
                <div className="session-library-actions">
                  <button type="button" data-action="rename" onClick={() => beginRename(item)}>Rename</button>
                  <button
                    type="button"
                    data-action={item.pinned ? "unpin" : "pin"}
                    onClick={() => onMutate(item.sessionId, { type: item.pinned ? "Unpin" : "Pin" })}
                  >
                    {item.pinned ? "Unpin" : "Pin"}
                  </button>
                  <button
                    type="button"
                    data-action={item.archived ? "restore" : "archive"}
                    onClick={() => onMutate(item.sessionId, { type: item.archived ? "Restore" : "Archive" })}
                  >
                    {item.archived ? "Restore" : "Archive"}
                  </button>
                </div>
              )}
            </li>
          ))}
        </ol>
      )}

      {status === "ready" && nextCursor && (
        <button type="button" data-action="load-more" onClick={() => onLoadMore(nextCursor)}>
          Load more
        </button>
      )}
    </section>
  );
}

function formatUpdatedAt(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? value : date.toLocaleString();
}
