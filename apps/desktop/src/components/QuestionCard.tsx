/**
 * A parked question, answerable.
 *
 * The desktop could raise an OS notification for `QuestionAsked` and then
 * showed a card with a title, the question text, no options, no input and no
 * buttons — while `CommandBody::ResolveQuestion` sat unused and the run stayed
 * blocked until someone answered from the TUI. This card is the TUI's question
 * modal (`crates/tui/src/render.rs::render_question_modal`) in web form: one
 * section per question with its options (radio or check boxes, as the prompt's
 * `multiple` says), a free-text answer when the prompt allows one (`custom`,
 * which a model can never disable), and a rejection with optional feedback.
 *
 * The outcome sent is the protocol's own `QuestionOutcome`: one list of chosen
 * labels per question, a custom answer carried verbatim as a label.
 */
import React, { useState } from "react";

import type { QuestionOutcomeView, QuestionPromptView } from "../types.js";

export interface QuestionCardProps {
  questionId: string;
  prompts: QuestionPromptView[];
  /** Absent when nothing can be sent (not connected, older shell). */
  onResolve?: (questionId: string, outcome: QuestionOutcomeView) => void;
}

const CARD: React.CSSProperties = {
  alignSelf: "flex-start",
  width: "85%",
  background: "var(--cody-panel-hover)",
  border: "1px solid var(--cody-accent)",
  borderRadius: 8,
  padding: 16,
};
const TITLE: React.CSSProperties = { fontWeight: 600, color: "var(--cody-link)", marginBottom: 8, fontSize: 14 };
const QUESTION: React.CSSProperties = { fontSize: 13, color: "var(--cody-text)", marginBottom: 8 };
const OPTION: React.CSSProperties = {
  display: "flex",
  alignItems: "baseline",
  gap: 8,
  fontSize: 13,
  color: "var(--cody-text)",
  padding: "3px 0",
  cursor: "pointer",
};
const DESCRIPTION: React.CSSProperties = { color: "var(--cody-text-muted)", fontSize: 12 };
const INPUT: React.CSSProperties = {
  width: "100%",
  boxSizing: "border-box",
  padding: "6px 10px",
  borderRadius: 6,
  border: "1px solid var(--cody-border-strong)",
  background: "var(--cody-canvas)",
  color: "var(--cody-text)",
  font: "inherit",
  fontSize: 13,
  marginTop: 6,
};
const ACTIONS: React.CSSProperties = { display: "flex", gap: 8, marginTop: 12, flexWrap: "wrap" };
const PRIMARY: React.CSSProperties = {
  background: "var(--cody-success-strong)",
  border: "none",
  color: "var(--cody-on-accent)",
  padding: "6px 12px",
  borderRadius: 6,
  fontSize: 12,
  cursor: "pointer",
  fontWeight: 600,
};
const SECONDARY: React.CSSProperties = {
  background: "var(--cody-inset)",
  border: "1px solid var(--cody-border-strong)",
  color: "var(--cody-text-secondary)",
  padding: "6px 12px",
  borderRadius: 6,
  fontSize: 12,
  cursor: "pointer",
};
const NOTE: React.CSSProperties = { fontSize: 12, color: "var(--cody-text-muted)", marginTop: 8 };

/** The answer to one question: the labels picked, plus any typed text. */
type Draft = { picked: string[]; custom: string };

export const QuestionCard: React.FC<QuestionCardProps> = ({ questionId, prompts, onResolve }) => {
  const [drafts, setDrafts] = useState<Draft[]>(() => prompts.map(() => ({ picked: [], custom: "" })));
  const [rejecting, setRejecting] = useState(false);
  const [feedback, setFeedback] = useState("");

  const update = (index: number, change: (draft: Draft) => Draft) => {
    setDrafts((current) => current.map((draft, at) => (at === index ? change(draft) : draft)));
  };

  /** One list of labels per question, the typed answer included verbatim. */
  const answers = drafts.map((draft) => {
    const custom = draft.custom.trim();
    return custom.length > 0 ? [...draft.picked, custom] : draft.picked;
  });
  const complete = answers.every((answer) => answer.length > 0);

  const answer = () => {
    if (!onResolve || !complete) {
      return;
    }
    onResolve(questionId, { type: "Answered", answers });
  };
  const reject = () => {
    if (!onResolve) {
      return;
    }
    const text = feedback.trim();
    onResolve(questionId, { type: "Rejected", ...(text.length > 0 ? { feedback: text } : {}) });
  };

  return (
    <div style={CARD} role="group" aria-label="Question from the agent" data-testid="question-card">
      <div style={TITLE}>
        {prompts.length > 1 ? `${prompts.length} questions from the agent` : "Question from the agent"}
      </div>
      {prompts.map((prompt, index) => {
        const draft = drafts[index] ?? { picked: [], custom: "" };
        const name = `question-${questionId}-${index}`;
        return (
          <section key={name} aria-label={prompt.header || `Question ${index + 1}`} style={{ marginBottom: 10 }}>
            {prompt.header && <div style={{ ...TITLE, fontSize: 13, marginBottom: 4 }}>{prompt.header}</div>}
            <div style={QUESTION}>{prompt.question}</div>
            {prompt.options.map((option) => {
              const checked = draft.picked.includes(option.label);
              return (
                <label key={option.label} style={OPTION}>
                  <input
                    type={prompt.multiple ? "checkbox" : "radio"}
                    name={name}
                    checked={checked}
                    onChange={() =>
                      update(index, (current) => ({
                        ...current,
                        picked: prompt.multiple
                          ? checked
                            ? current.picked.filter((label) => label !== option.label)
                            : [...current.picked, option.label]
                          : [option.label],
                      }))
                    }
                  />
                  <span>
                    {option.label}
                    {option.description && <span style={DESCRIPTION}> — {option.description}</span>}
                  </span>
                </label>
              );
            })}
            {prompt.custom && (
              <input
                type="text"
                aria-label={`Your own answer${prompt.header ? ` to ${prompt.header}` : ""}`}
                placeholder={prompt.options.length > 0 ? "Or type your own answer" : "Type your answer"}
                value={draft.custom}
                onChange={(event) => {
                  const custom = event.target.value;
                  update(index, (current) => ({ ...current, custom }));
                }}
                style={INPUT}
              />
            )}
          </section>
        );
      })}
      {onResolve ? (
        rejecting ? (
          <div>
            <input
              type="text"
              aria-label="Why you are rejecting this question (optional)"
              placeholder="Optional: tell the agent what to do instead"
              value={feedback}
              onChange={(event) => setFeedback(event.target.value)}
              style={INPUT}
            />
            <div style={ACTIONS}>
              <button type="button" style={PRIMARY} onClick={reject}>
                Send rejection
              </button>
              <button type="button" style={SECONDARY} onClick={() => setRejecting(false)}>
                Back
              </button>
            </div>
          </div>
        ) : (
          <div style={ACTIONS}>
            <button type="button" style={PRIMARY} disabled={!complete} onClick={answer}>
              Answer
            </button>
            <button type="button" style={SECONDARY} onClick={() => setRejecting(true)}>
              Reject
            </button>
          </div>
        )
      ) : (
        <div style={NOTE} role="status">
          The run is waiting on this answer. This client cannot send one right now — reconnect, or
          answer from the TUI.
        </div>
      )}
    </div>
  );
};
