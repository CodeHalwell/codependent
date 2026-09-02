import { describe, expect, it } from "vitest";
import { diagnoseFailure, sanitizeFailureText, summarizeError } from "../src/failure.js";
import { describeActivity, formatCostMicros, thousands, usageLabel } from "../src/runActivity.js";

describe("sanitizeFailureText", () => {
  // The TUI's `failure_sanitizer_redacts_common_credentials_and_is_bounded`,
  // ported: the same inputs must produce the same redactions.
  it("redacts common credentials and stays bounded", () => {
    const raw = `Authorization: Bearer abc123 api_key=sk-live password: hunter2 ghp_deadbeef ${"x".repeat(4096)}`;
    const safe = sanitizeFailureText(raw);
    for (const secret of ["abc123", "sk-live", "hunter2", "ghp_deadbeef"]) {
      expect(safe).not.toContain(secret);
    }
    expect(safe).toContain("[REDACTED]");
    expect(Array.from(safe).length).toBeLessThanOrEqual(2049);
    expect(safe.endsWith("…")).toBe(true);
  });

  it("redacts a credential inside a compact JSON body", () => {
    // No spaces to split on: the key-then-value rules never see the value, so
    // the whole word has to go on the key alone. An ACP agent or a proxy
    // returning its request headers is the realistic source.
    const safe = sanitizeFailureText(
      'model driver error: {"Authorization":"Bearer sk-live-abcdef123456"} rejected',
    );
    expect(safe).not.toContain("sk-live-abcdef123456");
    expect(safe).toContain("[REDACTED]");
    expect(safe).toContain("rejected");

    for (const body of [
      '{"x-api-key":"secret-value"}',
      '{"access_token":"secret-value"}',
      '{"refresh_token":"secret-value"}',
      '{"secret":"secret-value"}',
    ]) {
      expect(sanitizeFailureText(`failed with ${body}`)).not.toContain("secret-value");
    }
  });

  it("strips terminal escapes and bidi controls", () => {
    const safe = sanitizeFailureText("failed[31m hard‮ text");
    expect(safe).not.toContain("");
    expect(safe).not.toContain("‮");
    expect(safe).toBe("failed[31m hard text");
  });

  it("redacts an inline token= value but keeps the label", () => {
    expect(sanitizeFailureText("session/new failed token=super-secret")).toBe(
      "session/new failed token=[REDACTED]",
    );
  });
});

describe("summarizeError", () => {
  it("maps the known driver chains to one line", () => {
    expect(
      summarizeError("model driver error: model stream failed: service error: request failed: builder error"),
    ).toBe("model error — the provider request failed");
    expect(summarizeError("service error: something")).toBe("provider request failed");
  });

  it("surfaces a nested ACP details member", () => {
    expect(
      summarizeError('ACP prompt failed: session/new failed: {"details":"cline requires re-authentication"}'),
    ).toBe("ACP — cline requires re-authentication");
    expect(summarizeError("ACP prompt failed: internal error")).toBe(
      "ACP agent request failed — expand for details",
    );
  });

  it("degrades to the outermost segment, and to a fixed phrase when empty", () => {
    expect(summarizeError("pinned model openai/gpt-5 is not available: provider did not list this model")).toBe(
      "pinned model openai/gpt-5 is not available",
    );
    expect(summarizeError("")).toBe("run failed");
  });
});

describe("diagnoseFailure", () => {
  it("points a 401 at the API keys surface", () => {
    const diagnosis = diagnoseFailure(
      'model driver error: OpenAI-compatible API error 401 Unauthorized: {"error":{"message":"Incorrect API key sk-abc"}}',
    );
    expect(diagnosis.remedy).toBe("keys");
    expect(diagnosis.authRelated).toBe(true);
    expect(diagnosis.detail).not.toContain("sk-abc");
    expect(diagnosis.hint).toMatch(/API Keys/);
  });

  it("points a rate limit at retrying", () => {
    expect(diagnoseFailure("provider request failed: 429 Too Many Requests").remedy).toBe("retry");
  });

  it("points a missing model at the models surface", () => {
    expect(diagnoseFailure("no model configured (no models.toml)").remedy).toBe("models");
  });

  it("points an unreachable endpoint at the connection", () => {
    expect(diagnoseFailure("connection check to http://localhost:11434/v1 failed: connection refused").remedy).toBe(
      "daemon",
    );
  });

  it("offers nothing it cannot justify", () => {
    const diagnosis = diagnoseFailure("the agent gave up");
    expect(diagnosis.remedy).toBe("none");
    expect(diagnosis.hint).toBeNull();
  });
});

describe("usage and activity labels", () => {
  it("formats the usage chip exactly as the TUI does", () => {
    expect(thousands(1234)).toBe("1,234");
    expect(formatCostMicros(3400)).toBe("$0.0034");
    expect(formatCostMicros(2_500_000)).toBe("$2.5000");
    expect(
      usageLabel({ runId: "r", promptTokens: 1234, completionTokens: 567, costMicros: 3400 }),
    ).toBe("1,234 in · 567 out · $0.0034");
  });

  it("never invents a price for an unpriced run", () => {
    expect(usageLabel({ runId: "r", promptTokens: 10, completionTokens: null, costMicros: null })).toBe(
      "10 in",
    );
    expect(usageLabel({ runId: "r", promptTokens: null, completionTokens: null, costMicros: null })).toBeNull();
    expect(usageLabel(null)).toBeNull();
  });

  it("describes every activity but idle and streaming", () => {
    expect(describeActivity({ kind: "idle" })).toBeNull();
    expect(describeActivity({ kind: "streaming" })).toBeNull();
    expect(describeActivity({ kind: "thinking" })).toBe("working…");
    expect(describeActivity({ kind: "tool", tool: "shell.run" })).toBe("running shell.run…");
    expect(describeActivity({ kind: "waiting", on: "approval" })).toBe("waiting for your approval");
    expect(
      describeActivity({
        kind: "retrying",
        attempt: 2,
        maxAttempts: 5,
        message: "provider is overloaded",
        delayMs: 4231,
      }),
    ).toBe("retrying (2/5) · provider is overloaded · next attempt in 4s");
  });
});

describe("diagnoseFailure with the daemon's structured error", () => {
  it("lets a typed user_action outrank the text heuristics", () => {
    // The text alone says nothing about authentication; the daemon's
    // classification does, and it wins.
    const auth = diagnoseFailure("model driver error: service error: request failed", {
      code: "model.invalid-auth",
      message: "the provider refused the credential: invalid x-api-key",
      retryable: false,
      user_action: { type: "Reauthenticate" },
    });
    expect(auth.remedy).toBe("keys");
    expect(auth.authRelated).toBe(true);
    expect(auth.summary).toContain("refused the credential");

    const model = diagnoseFailure("model driver error: boom", {
      code: "model.unreachable",
      user_action: { type: "ReconfigureModel" },
    });
    expect(model.remedy).toBe("models");

    const budget = diagnoseFailure("wall-clock budget exhausted", {
      code: "run.wall-clock-exhausted",
      user_action: { type: "AdjustPolicy" },
    });
    expect(budget.remedy).toBe("retry");
    expect(budget.hint).toContain("budget");
  });

  it("falls back to the text when the action is absent or unknown", () => {
    const plain = diagnoseFailure("service error: 401 unauthorized", {
      code: "model.error",
      user_action: { type: "SomethingNewer" },
    });
    expect(plain.remedy).toBe("keys");
    expect(diagnoseFailure("service error: 401 unauthorized").remedy).toBe("keys");
    expect(diagnoseFailure("nothing useful", { code: "x" }).remedy).toBe("none");
  });
});

describe("every authorization scheme, not just Bearer", () => {
  // `Bearer` was caught only because the key word happens to END with it. Any
  // other scheme puts a space between the key word and the credential, and
  // redacting the key word alone left the value on screen.
  it.each(["Basic", "Digest", "Negotiate", "Token", "DPoP", "Bearer"])(
    "redacts a %s credential in a compact JSON body",
    (scheme) => {
      const safe = sanitizeFailureText(
        `driver error: {"Authorization":"${scheme} dXNlcjpwYXNzd29yZA=="} rejected`,
      );
      expect(safe).not.toContain("dXNlcjpwYXNzd29yZA==");
      expect(safe).toContain("rejected");
    },
  );

  it("stops redacting at the end of the credential value", () => {
    const safe = sanitizeFailureText(
      '{"password":"two words"} and then a great deal of ordinary explanation',
    );
    expect(safe).not.toContain("two words");
    expect(safe).toContain("ordinary explanation");
  });

  it("bounds an unterminated value rather than swallowing the message", () => {
    const safe = sanitizeFailureText(
      '{"password":"never closed and on it goes one two three four five six seven eight nine ten eleven',
    );
    expect(safe).toContain("eleven");
  });
});

describe("a multi-parameter authorization header", () => {
  it("redacts the whole value, not a fixed number of words", () => {
    // A two-word budget spent itself on the scheme and the first parameter,
    // leaving the nonce and the response on screen. A header value ends at its
    // line, so redaction runs to the end of it.
    const safe = sanitizeFailureText(
      'upstream said:\nAuthorization: Digest username="u", realm="r", nonce="n0nce", response="s3cret"\nrequest rejected',
    );
    expect(safe).not.toContain("n0nce");
    expect(safe).not.toContain("s3cret");
    expect(safe).not.toContain('realm="r"');
    // The lines either side are untouched: the header ended at its own.
    expect(safe).toContain("upstream said:");
    expect(safe).toContain("request rejected");
  });
});

describe("a compact JSON credential with escaped quotes", () => {
  it("redacts past the escaped quotes inside the value", () => {
    // `\"` is part of the value, not its end. Counting raw quotes stopped at
    // the first escaped one and printed the realm and the response.
    const safe = sanitizeFailureText(
      'upstream 500: {"Authorization":"Digest username=\\"u\\", realm=\\"r\\", nonce=\\"n0nce\\", response=\\"s3cret\\""}',
    );
    expect(safe).not.toContain("n0nce");
    expect(safe).not.toContain("s3cret");
    expect(safe).not.toContain('realm=\\"r\\"');
    expect(safe).toContain("upstream 500:");
  });
});
