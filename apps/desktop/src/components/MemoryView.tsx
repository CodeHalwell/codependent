/**
 * Memory + Journey — the TUI's `Overlay::Memory` and `Overlay::Journey`.
 *
 * Two lists, two data paths, and they are NOT interchangeable:
 *
 * - Memories are read from the daemon's SQLite (there is no `ListMemories` on
 *   the wire) but corrected and forgotten over the PROTOCOL
 *   (`CorrectMemory` / `ForgetMemory`, both keyed by `{ id, repository }`).
 * - Learnings are read AND mutated in SQLite with an optimistic revision;
 *   the store answers `Conflict` / `Duplicate`, which are surfaced as
 *   outcomes rather than swallowed.
 *
 * "Open source" does no I/O: it reveals the evidence string already loaded
 * with the card, exactly as `state.rs` describes.
 */
import React, { useState } from "react";
import type {
  Loaded,
  LearningCard,
  LearningMutation,
  MemoryCard,
} from "./knowledgeTransport.js";
import { ConfirmPanel, Field, SurfaceBody, surfaceButton, surfaceStyles } from "./surfaceChrome.js";

export interface MemoryViewProps {
  memories: Loaded<MemoryCard>;
  learnings: Loaded<LearningCard>;
  /** Present only when the shell can actually serve the read. */
  onRefresh?: () => void;
  /** `CorrectMemory`. Absent when the bridge command does not exist. */
  onCorrectMemory?: (memoryId: string, statement: string) => void;
  /** `ForgetMemory`. Absent when the bridge command does not exist. */
  onForgetMemory?: (memoryId: string) => void;
  /** Optimistic-revision learning mutation. */
  onMutateLearning?: (id: string, revision: number, mutation: LearningMutation) => void;
  /** The last outcome sentence from a mutation, shown verbatim. */
  notice?: string | null;
}

type Pending =
  | { kind: "forget"; memoryId: string; statement: string }
  | { kind: "deleteLearning"; id: string; revision: number; statement: string };

export const MemoryView: React.FC<MemoryViewProps> = ({
  memories,
  learnings,
  onRefresh,
  onCorrectMemory,
  onForgetMemory,
  onMutateLearning,
  notice,
}) => {
  const [tab, setTab] = useState<"memories" | "journey">("memories");
  const [openSource, setOpenSource] = useState<string | null>(null);
  const [correcting, setCorrecting] = useState<{ id: string; buffer: string } | null>(null);
  const [editing, setEditing] = useState<{ id: string; revision: number; buffer: string } | null>(
    null,
  );
  const [pending, setPending] = useState<Pending | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);

  const tabStyle = (active: boolean): React.CSSProperties => ({
    ...surfaceButton(),
    background: active ? "#1f242c" : "#21262d",
    borderColor: active ? "#388bfd" : "#30363d",
    fontWeight: active ? 600 : 400,
  });

  return (
    <div style={surfaceStyles.page}>
      <div style={surfaceStyles.header}>
        <div>
          <div style={surfaceStyles.title}>Memory</div>
          <div style={surfaceStyles.subtitle}>
            Curated memories and their provenance; governed learnings awaiting review.
          </div>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button style={tabStyle(tab === "memories")} onClick={() => setTab("memories")}>
            Memories
          </button>
          <button style={tabStyle(tab === "journey")} onClick={() => setTab("journey")}>
            Journey
          </button>
          {onRefresh && (
            <button style={surfaceButton()} onClick={onRefresh}>
              Refresh
            </button>
          )}
        </div>
      </div>

      <div style={surfaceStyles.scroll}>
        {notice && (
          <div role="status" style={{ ...surfaceStyles.card, color: "#7ee787", borderColor: "#238636" }}>
            {notice}
          </div>
        )}
        {refusal && (
          <div role="alert" style={{ ...surfaceStyles.card, color: "#ffa198", borderColor: "#da3633" }}>
            {refusal}
          </div>
        )}

        {pending?.kind === "forget" && (
          <ConfirmPanel
            title="Forget this memory?"
            evidence={pending.statement}
            confirmLabel="Forget"
            onConfirm={() => {
              onForgetMemory?.(pending.memoryId);
              setPending(null);
            }}
            onCancel={() => setPending(null)}
          />
        )}
        {pending?.kind === "deleteLearning" && (
          <ConfirmPanel
            title="Permanently delete this learning?"
            evidence={pending.statement}
            confirmLabel="Delete"
            onConfirm={() => {
              onMutateLearning?.(pending.id, pending.revision, { type: "Delete" });
              setPending(null);
            }}
            onCancel={() => setPending(null)}
          />
        )}

        {tab === "memories" ? (
          <SurfaceBody
            status={memories.status}
            detail={memories.detail}
            count={memories.items.length}
            emptyMessage={
              "Nothing has been remembered yet. Memory is harvested after each run, and " +
              "extraction needs a model that can act as a chat client — an ACP entry cannot, " +
              "being a full-agent executor. If every model in models.toml is ACP, add one " +
              "chat-capable model (a local Ollama or any OpenAI-compatible endpoint will do) " +
              "or set `memory_extraction_model` in routing.toml, and the next run will start " +
              "filling this."
            }
          >
            {memories.items.map((memory) => (
              <div key={memory.id} style={surfaceStyles.card}>
                <div style={{ fontSize: 13, color: "#e6edf3" }}>{memory.statement}</div>
                <div style={{ marginTop: 8 }}>
                  <Field label="class" value={memory.class} />
                  <Field label="scope" value={memory.scope} />
                  <Field label="from" value={memory.revision} />
                  <Field label="observed" value={memory.observed} />
                  <Field label="confidence" value={memory.confidence.toFixed(2)} />
                </div>
                <div style={{ display: "flex", gap: 8, marginTop: 10, flexWrap: "wrap" }}>
                  <button
                    style={surfaceButton()}
                    title="Reveal the evidence this memory was curated from — already loaded, no file is opened"
                    onClick={() => setOpenSource(openSource === memory.id ? null : memory.id)}
                  >
                    {openSource === memory.id ? "Hide source" : "Open source"}
                  </button>
                  {onCorrectMemory && (
                    <button
                      style={surfaceButton()}
                      onClick={() =>
                        setCorrecting({ id: memory.id, buffer: memory.statement })
                      }
                    >
                      Correct
                    </button>
                  )}
                  {onForgetMemory && (
                    <button
                      style={surfaceButton("danger")}
                      onClick={() =>
                        setPending({
                          kind: "forget",
                          memoryId: memory.id,
                          statement: memory.statement,
                        })
                      }
                    >
                      Forget
                    </button>
                  )}
                </div>
                {openSource === memory.id && (
                  <pre
                    style={{
                      ...surfaceStyles.mono,
                      whiteSpace: "pre-wrap",
                      wordBreak: "break-word",
                      color: "#8b949e",
                      marginTop: 10,
                      marginBottom: 0,
                    }}
                  >
                    {memory.source}
                  </pre>
                )}
                {correcting?.id === memory.id && onCorrectMemory && (
                  <div style={{ marginTop: 10 }}>
                    <textarea
                      value={correcting.buffer}
                      aria-label="Corrected statement"
                      rows={3}
                      onChange={(event) =>
                        setCorrecting({ id: memory.id, buffer: event.target.value })
                      }
                      style={{
                        width: "100%",
                        boxSizing: "border-box",
                        background: "#0d1117",
                        border: "1px solid #30363d",
                        borderRadius: 6,
                        color: "#e6edf3",
                        fontSize: 12,
                        padding: 8,
                      }}
                    />
                    <div style={{ display: "flex", gap: 8, marginTop: 6 }}>
                      <button
                        style={surfaceButton("primary")}
                        onClick={() => {
                          const statement = correcting.buffer.trim();
                          if (statement.length === 0) {
                            // Blank refuses; nothing is sent and the prompt stays open.
                            setRefusal("a corrected statement must not be empty");
                            return;
                          }
                          setRefusal(null);
                          onCorrectMemory(memory.id, statement);
                          setCorrecting(null);
                        }}
                      >
                        Send correction
                      </button>
                      <button style={surfaceButton()} onClick={() => setCorrecting(null)}>
                        Cancel
                      </button>
                    </div>
                  </div>
                )}
              </div>
            ))}
          </SurfaceBody>
        ) : (
          <SurfaceBody
            status={learnings.status}
            detail={learnings.detail}
            count={learnings.items.length}
            emptyMessage="No proposed or active learnings for this repository."
          >
            {learnings.items.map((learning) => (
              <div key={learning.id} style={surfaceStyles.card}>
                <div style={{ fontSize: 13, color: "#e6edf3" }}>{learning.statement}</div>
                <div style={{ marginTop: 8 }}>
                  <Field label="kind" value={learning.kind} />
                  <Field label="state" value={learning.state} />
                  <Field label="scope" value={learning.scope} />
                  <Field label="provenance" value={learning.provenance} />
                  <Field label="confidence" value={learning.confidence.toFixed(2)} />
                  <Field label="rev" value={learning.revision} />
                  {learning.pinned && <Field label="" value="pinned" />}
                </div>
                {onMutateLearning && (
                  <div style={{ display: "flex", gap: 8, marginTop: 10, flexWrap: "wrap" }}>
                    <button
                      style={surfaceButton()}
                      onClick={() =>
                        onMutateLearning(learning.id, learning.revision, { type: "Activate" })
                      }
                    >
                      Activate
                    </button>
                    <button
                      style={surfaceButton()}
                      onClick={() =>
                        onMutateLearning(learning.id, learning.revision, { type: "Reject" })
                      }
                    >
                      Reject
                    </button>
                    <button
                      style={surfaceButton()}
                      onClick={() =>
                        onMutateLearning(learning.id, learning.revision, {
                          type: "SetPinned",
                          pinned: !learning.pinned,
                        })
                      }
                    >
                      {learning.pinned ? "Unpin" : "Pin"}
                    </button>
                    <button
                      style={surfaceButton()}
                      onClick={() =>
                        setEditing({
                          id: learning.id,
                          revision: learning.revision,
                          buffer: learning.statement,
                        })
                      }
                    >
                      Edit
                    </button>
                    <button
                      style={surfaceButton("danger")}
                      onClick={() =>
                        setPending({
                          kind: "deleteLearning",
                          id: learning.id,
                          revision: learning.revision,
                          statement: learning.statement,
                        })
                      }
                    >
                      Delete
                    </button>
                  </div>
                )}
                {editing?.id === learning.id && onMutateLearning && (
                  <div style={{ marginTop: 10 }}>
                    <textarea
                      value={editing.buffer}
                      aria-label="Edited learning statement"
                      rows={3}
                      onChange={(event) =>
                        setEditing({ ...editing, buffer: event.target.value })
                      }
                      style={{
                        width: "100%",
                        boxSizing: "border-box",
                        background: "#0d1117",
                        border: "1px solid #30363d",
                        borderRadius: 6,
                        color: "#e6edf3",
                        fontSize: 12,
                        padding: 8,
                      }}
                    />
                    <div style={{ display: "flex", gap: 8, marginTop: 6 }}>
                      <button
                        style={surfaceButton("primary")}
                        onClick={() => {
                          const statement = editing.buffer.trim();
                          if (statement.length === 0) {
                            setRefusal("a learning statement must not be empty");
                            return;
                          }
                          setRefusal(null);
                          onMutateLearning(editing.id, editing.revision, {
                            type: "EditStatement",
                            statement,
                          });
                          setEditing(null);
                        }}
                      >
                        Save
                      </button>
                      <button style={surfaceButton()} onClick={() => setEditing(null)}>
                        Cancel
                      </button>
                    </div>
                  </div>
                )}
              </div>
            ))}
          </SurfaceBody>
        )}
      </div>
    </div>
  );
};
