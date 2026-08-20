import React from "react";

/**
 * The little of Markdown that model output actually uses, rendered to React
 * elements.
 *
 * **No HTML passthrough, by construction.** Every node here is a React element
 * built from parsed text; nothing is ever handed to `dangerouslySetInnerHTML`.
 * The content is model output arriving over a socket, so a renderer that could
 * emit arbitrary HTML into a webview would be an injection vector — which is
 * also why this is a hundred lines rather than a dependency.
 *
 * Supported, because it is what the models emit: ATX headings, fenced code
 * blocks, unordered and ordered lists, blockquotes, horizontal rules, and
 * inline `code`, `**bold**`, `*italic*` and `[text](href)` — links render as
 * their text plus the href, never as a navigable anchor.
 *
 * Anything unrecognised is left exactly as written, which is the important
 * property: unsupported syntax degrades to the plain text it already was
 * rather than disappearing.
 */

const CODE_FONT = "ui-monospace, SFMono-Regular, Menlo, monospace";

const H_STYLES: Record<number, React.CSSProperties> = {
  1: { fontSize: 18, fontWeight: 700, margin: "12px 0 6px" },
  2: { fontSize: 16, fontWeight: 700, margin: "12px 0 6px" },
  3: { fontSize: 15, fontWeight: 600, margin: "10px 0 4px" },
  4: { fontSize: 14, fontWeight: 600, margin: "10px 0 4px" },
  5: { fontSize: 13, fontWeight: 600, margin: "8px 0 4px" },
  6: { fontSize: 13, fontWeight: 600, margin: "8px 0 4px", color: "#8b949e" },
};

const CODE_BLOCK: React.CSSProperties = {
  margin: "8px 0",
  padding: "10px 12px",
  background: "#0d1117",
  border: "1px solid #30363d",
  borderRadius: 6,
  fontFamily: CODE_FONT,
  fontSize: 12,
  overflowX: "auto",
  whiteSpace: "pre",
};

const INLINE_CODE: React.CSSProperties = {
  fontFamily: CODE_FONT,
  fontSize: "0.92em",
  background: "#0d1117",
  border: "1px solid #30363d",
  borderRadius: 4,
  padding: "1px 4px",
};

const QUOTE: React.CSSProperties = {
  margin: "8px 0",
  padding: "2px 0 2px 12px",
  borderLeft: "3px solid #30363d",
  color: "#8b949e",
};

const LIST: React.CSSProperties = { margin: "6px 0", paddingLeft: 22 };
const PARAGRAPH: React.CSSProperties = { margin: "6px 0", whiteSpace: "pre-wrap" };
const RULE: React.CSSProperties = { border: 0, borderTop: "1px solid #30363d", margin: "12px 0" };
const LINK: React.CSSProperties = { color: "#58a6ff" };

/** `**bold**`, `*italic*`, `` `code` `` and `[text](href)`, in one pass. */
const INLINE = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*\n]+\*)|(\[[^\]]+\]\([^)\s]+\))/g;

function inline(text: string, key: string): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  let last = 0;
  let index = 0;
  for (const match of text.matchAll(INLINE)) {
    const at = match.index;
    if (at > last) {
      out.push(text.slice(last, at));
    }
    const token = match[0];
    const id = `${key}-i${index++}`;
    if (token.startsWith("`")) {
      out.push(
        <code key={id} style={INLINE_CODE}>
          {token.slice(1, -1)}
        </code>,
      );
    } else if (token.startsWith("**")) {
      // Recursed, so `**`code`**` renders as bold code rather than bold
      // backticks. The slice is strictly shorter, so this terminates.
      out.push(<strong key={id}>{inline(token.slice(2, -2), id)}</strong>);
    } else if (token.startsWith("[")) {
      // Rendered as text plus its target, never as a navigable anchor: a click
      // target the model chose is not one the operator asked for.
      const split = token.indexOf("](");
      out.push(
        <span key={id} style={LINK}>
          {token.slice(1, split)} ({token.slice(split + 2, -1)})
        </span>,
      );
    } else {
      out.push(<em key={id}>{inline(token.slice(1, -1), id)}</em>);
    }
    last = at + token.length;
  }
  if (last < text.length) {
    out.push(text.slice(last));
  }
  return out;
}

/** Render `text` as Markdown. Returns React elements; never HTML. */
export function renderMarkdown(text: string): React.ReactNode {
  const lines = text.split("\n");
  const blocks: React.ReactNode[] = [];
  let paragraph: string[] = [];
  let key = 0;

  const flush = () => {
    if (paragraph.length > 0) {
      const body = paragraph.join("\n");
      blocks.push(
        <div key={`p${key++}`} style={PARAGRAPH}>
          {inline(body, `p${key}`)}
        </div>,
      );
      paragraph = [];
    }
  };

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];

    const fence = /^\s*```(\w*)\s*$/.exec(line);
    if (fence) {
      flush();
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !/^\s*```\s*$/.test(lines[i])) {
        body.push(lines[i]);
        i += 1;
      }
      blocks.push(
        <pre key={`c${key++}`} style={CODE_BLOCK}>
          {body.join("\n")}
        </pre>,
      );
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      flush();
      const level = heading[1].length;
      blocks.push(
        <div key={`h${key++}`} style={H_STYLES[level]}>
          {inline(heading[2], `h${key}`)}
        </div>,
      );
      continue;
    }

    if (/^\s*(---+|\*\*\*+|___+)\s*$/.test(line)) {
      flush();
      blocks.push(<hr key={`r${key++}`} style={RULE} />);
      continue;
    }

    if (/^\s*>\s?/.test(line)) {
      flush();
      const body: string[] = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
        body.push(lines[i].replace(/^\s*>\s?/, ""));
        i += 1;
      }
      i -= 1;
      blocks.push(
        <div key={`q${key++}`} style={QUOTE}>
          {inline(body.join("\n"), `q${key}`)}
        </div>,
      );
      continue;
    }

    if (/^\s*([-*+]|\d+\.)\s+/.test(line)) {
      flush();
      const ordered = /^\s*\d+\./.test(line);
      const items: string[] = [];
      while (i < lines.length && /^\s*([-*+]|\d+\.)\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*([-*+]|\d+\.)\s+/, ""));
        i += 1;
      }
      i -= 1;
      const children = items.map((item, at) => (
        <li key={`li${at}`}>{inline(item, `l${key}-${at}`)}</li>
      ));
      blocks.push(
        ordered ? (
          <ol key={`o${key++}`} style={LIST}>
            {children}
          </ol>
        ) : (
          <ul key={`u${key++}`} style={LIST}>
            {children}
          </ul>
        ),
      );
      continue;
    }

    if (line.trim() === "") {
      flush();
      continue;
    }
    paragraph.push(line);
  }
  flush();
  return <>{blocks}</>;
}
