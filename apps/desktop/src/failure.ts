/**
 * What a failed run says to the operator.
 *
 * A run's failure `reason` is whatever the provider, driver, or ACP agent put
 * on the wire: a nested error chain, a JSON-RPC body, terminal escapes, and —
 * from a misconfigured proxy or an agent echoing its own request — the very
 * credential that failed. The TUI never renders that raw
 * (`crates/tui/src/state.rs::sanitize_failure_text`, `render.rs::summarize_error`);
 * this module is the same treatment for the desktop, ported rule for rule so
 * both clients tell the same story about the same failure.
 */

/** The hard cap on rendered failure text, matching the TUI's `MAX_CHARS`. */
const MAX_CHARS = 2_048;

const BIDI_CONTROLS = new Set([
  "؜",
  "‎",
  "‏",
  "‪",
  "‫",
  "‬",
  "‭",
  "‮",
  "⁦",
  "⁧",
  "⁨",
  "⁩",
]);

function isControl(character: string): boolean {
  const code = character.codePointAt(0) ?? 0;
  return code < 0x20 || (code >= 0x7f && code <= 0x9f);
}

/**
 * Strip terminal-control payloads and credential-shaped values from an
 * untrusted provider/agent error before it is rendered or copied. Bounded.
 */
export function sanitizeFailureText(raw: string): string {
  const cleaned = Array.from(raw)
    .filter(
      (character) =>
        character === "\n" ||
        character === "\t" ||
        (!isControl(character) && !BIDI_CONTROLS.has(character)),
    )
    .slice(0, MAX_CHARS)
    .join("");

  const words: string[] = [];
  // Whitespace-delimited fields still owed to a credential header:
  // `Authorization: Bearer <token>` needs two, `password: <value>` needs one.
  let redactFollowing = 0;
  for (const word of cleaned.split(/\s+/).filter((part) => part.length > 0)) {
    const lower = word.toLowerCase();
    const label = lower.replace(/^[^a-z0-9]+|[^a-z0-9]+$/g, "");
    if (redactFollowing > 0) {
      words.push("[REDACTED]");
      redactFollowing -= 1;
      continue;
    }
    if (label === "authorization" || lower.endsWith("authorization:")) {
      words.push(word);
      redactFollowing = 2;
      continue;
    }
    const credentialKeywords = ["bearer", "token", "api_key", "apikey", "password"];
    if (
      credentialKeywords.includes(label) ||
      lower.endsWith("api-key:") ||
      // A JSON value carries spaces (`"Authorization":"Bearer abc123"`), so
      // the word naming the key holds only the START of it and the credential
      // is the NEXT word. Matching the label's TAIL catches that:
      // `{"authorization":"bearer` ends with `bearer`. Redaction errs safe, so
      // an over-eager match costs a word of context.
      credentialKeywords.some((keyword) => label.endsWith(keyword))
    ) {
      words.push(word);
      redactFollowing = 1;
      continue;
    }
    const secretPrefix = ["sk-", "ghp_", "github_pat_", "xoxb-", "xoxp-", "tvly-"].some((prefix) =>
      lower.startsWith(prefix),
    );
    // A compact JSON body is ONE whitespace-delimited word, so the key-then-
    // value rules above never see the value: `{"Authorization":"Bearer secret"}`
    // has no space to split on, does not start with a known prefix, and the
    // whole word must therefore be redacted on the key alone. `authorization`
    // is the one that carries a live credential most often and was missing.
    const jsonSecret = [
      '"authorization":',
      '"token":',
      '"access_token":',
      '"refresh_token":',
      '"api_key":',
      '"apikey":',
      '"x-api-key":',
      '"secret":',
      '"password":',
    ].some((needle) => lower.includes(needle));
    let inline: { needle: string; index: number } | null = null;
    for (const needle of ["token=", "api_key=", "apikey=", "password=", "bearer="]) {
      const index = lower.indexOf(needle);
      if (index !== -1) {
        inline = { needle, index };
        break;
      }
    }
    if (secretPrefix || jsonSecret) {
      words.push("[REDACTED]");
    } else if (inline) {
      words.push(`${word.slice(0, inline.index + inline.needle.length)}[REDACTED]`);
    } else {
      words.push(word);
    }
  }

  let safe = words.join(" ");
  if (Array.from(safe).length > MAX_CHARS) {
    safe = Array.from(safe).slice(0, MAX_CHARS).join("");
  }
  if (Array.from(raw).length > MAX_CHARS) {
    if (Array.from(safe).length === MAX_CHARS) {
      safe = Array.from(safe).slice(0, -1).join("");
    }
    safe += "…";
  }
  return safe;
}

/**
 * One line a person can act on, from a raw error chain. Mirrors the TUI's
 * `summarize_error`: an ACP `details` member wins, then the recognised driver
 * categories, then the outermost segment of the chain.
 */
export function summarizeError(raw: string): string {
  const lower = raw.toLowerCase();
  if (lower.includes("acp")) {
    const marker = raw.indexOf('"details"');
    if (marker !== -1) {
      const tail = raw.slice(marker + '"details"'.length);
      const colon = tail.indexOf(":");
      if (colon !== -1) {
        const value = tail.slice(colon + 1).trim();
        if (value.startsWith('"')) {
          const detail = value.slice(1).split('"')[0].replace(/\\n/g, " ").trim();
          if (detail.length > 0) {
            return `ACP — ${detail}`;
          }
        }
      }
    }
    if (lower.includes("prompt failed")) {
      return "ACP agent request failed — expand for details";
    }
  }
  const segments = raw.split(": ").map((segment) => segment.trim());
  const outer = segments[0] ?? "";
  if (segments.some((segment) => segment === "model driver error" || segment === "model stream failed")) {
    return "model error — the provider request failed";
  }
  if (segments.some((segment) => segment === "service error" || segment === "request failed")) {
    return "provider request failed";
  }
  return outer.length === 0 ? "run failed" : outer;
}

/** Where the operator should go next, when the failure text says. */
export type FailureRemedy = "keys" | "models" | "retry" | "daemon" | "none";

export interface FailureDiagnosis {
  /** The one-line summary, safe to render. */
  summary: string;
  /** The full chain, sanitised. Never the raw reason. */
  detail: string;
  /** Whether the text names an authentication problem. */
  authRelated: boolean;
  remedy: FailureRemedy;
  /** A sentence naming the next step, or `null` when there is nothing to add. */
  hint: string | null;
}

/**
 * Read a failure reason and say what to do about it.
 *
 * The classification is by substring, exactly as the TUI's failure card
 * decides whether to offer `Alt-A re-authenticate`. It is a hint, never a
 * verdict: the full sanitised text stays a click away on the card.
 */
/**
 * The structured half of a failed run, when the daemon sent one: the
 * protocol's `CodypendentError` beside `RunDisposition::Failed.reason`.
 * Only the fields the diagnosis reads are typed here.
 */
export interface StructuredFailure {
  code?: string;
  message?: string;
  retryable?: boolean;
  user_action?: { type?: string } | null;
}

/**
 * The remedy a structured `user_action` names, or `null` when the daemon sent
 * none (or one this build does not know), in which case the text heuristics
 * below decide — exactly as they did before the field existed.
 */
function structuredRemedy(error: StructuredFailure | undefined): Pick<FailureDiagnosis, "remedy" | "hint" | "authRelated"> | null {
  switch (error?.user_action?.type) {
    case "Reauthenticate":
      return {
        authRelated: true,
        remedy: "keys",
        hint: "The provider refused the credential. Check the key under API Keys, then retry.",
      };
    case "ReconfigureModel":
      return {
        authRelated: false,
        remedy: "models",
        hint: "The model or its endpoint needs attention. Check it under Models, or choose another.",
      };
    case "Retry":
      return {
        authRelated: false,
        remedy: "retry",
        hint: "The provider could not serve the request just now. Wait a moment and retry.",
      };
    case "AdjustPolicy":
      return {
        authRelated: false,
        remedy: "retry",
        hint: "The run hit a budget. Retry starts a fresh one; widen the policy's budgets for more room.",
      };
    default:
      return null;
  }
}

export function diagnoseFailure(reason: string, error?: StructuredFailure): FailureDiagnosis {
  const detail = sanitizeFailureText(reason);
  const summary = summarizeError(detail);
  const structured = structuredRemedy(error);
  if (structured) {
    // The daemon classified the typed cause; that outranks any reading of
    // the text. Its own message is the better summary when it has one.
    return {
      summary: error?.message ? sanitizeFailureText(error.message) : summary,
      detail,
      ...structured,
    };
  }
  const lower = detail.toLowerCase();
  const authRelated =
    ["auth", "login", "credential", "unauthorized", "401", "403", "invalid x-api-key", "api key"].some(
      (needle) => lower.includes(needle),
    );
  if (authRelated) {
    return {
      summary,
      detail,
      authRelated,
      remedy: "keys",
      hint: "The provider refused the credential. Check the key under API Keys, then retry.",
    };
  }
  if (["429", "rate limit", "rate_limit", "overloaded", "quota", "too many requests"].some((needle) => lower.includes(needle))) {
    return {
      summary,
      detail,
      authRelated,
      remedy: "retry",
      hint: "The provider is rate-limiting or overloaded. Wait a moment and retry, or choose another model.",
    };
  }
  if (["not registered", "no candidate model", "not configured", "unknown model", "no model"].some((needle) => lower.includes(needle))) {
    return {
      summary,
      detail,
      authRelated,
      remedy: "models",
      hint: "No usable model is configured for this run. Add or choose one under Models.",
    };
  }
  if (["connection refused", "dns", "timed out", "timeout", "could not connect", "connect error", "network"].some((needle) => lower.includes(needle))) {
    return {
      summary,
      detail,
      authRelated,
      remedy: "daemon",
      hint: "The endpoint could not be reached. Check the base URL, that a local server is running, and your network.",
    };
  }
  return { summary, detail, authRelated, remedy: "none", hint: null };
}
