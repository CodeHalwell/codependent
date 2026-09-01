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
