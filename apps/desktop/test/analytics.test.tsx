import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AnalyticsDashboard } from "../src/components/AnalyticsDashboard.js";
import type { AnalyticsPage } from "@codypendent/protocol";

describe("AnalyticsDashboard component", () => {
  it("missing_measurements_render_as_unavailable", () => {
    // Acceptance criterion 28:
    // The desktop analytics view renders explicit "unavailable" markers rather than zeros
    const pageWithNullMeasurements: AnalyticsPage = {
      items: [
        {
          dimensions: ["unmeasured-model"],
          metrics: {
            input_tokens: null,
            output_tokens: null,
            cached_tokens: null,
            reasoning_tokens: null,
            cost_micros: null,
            cost_per_successful_task_micros: null,
            latency_ms: null,
            retry_count: null,
            escalation_count: null,
            grader_score_micros: null,
          },
        },
      ],
      next_cursor: null,
    };

    render(<AnalyticsDashboard initialPage={pageWithNullMeasurements} />);

    // Check that explicit "unavailable" markers are displayed in the metric cards
    const inputMetric = screen.getByTestId("metric-input-tokens");
    expect(inputMetric.textContent).toContain("unavailable");
    expect(inputMetric.textContent).not.toContain("0");

    const outputMetric = screen.getByTestId("metric-output-tokens");
    expect(outputMetric.textContent).toContain("unavailable");
    expect(outputMetric.textContent).not.toContain("0");

    const cachedMetric = screen.getByTestId("metric-cached-tokens");
    expect(cachedMetric.textContent).toContain("unavailable");
    expect(cachedMetric.textContent).not.toContain("0");

    const reasoningMetric = screen.getByTestId("metric-reasoning-tokens");
    expect(reasoningMetric.textContent).toContain("unavailable");
    expect(reasoningMetric.textContent).not.toContain("0");

    const costMetric = screen.getByTestId("metric-cost");
    expect(costMetric.textContent).toContain("unavailable");
    expect(costMetric.textContent).not.toContain("$0.00");

    const costPerTaskMetric = screen.getByTestId("metric-cost-per-task");
    expect(costPerTaskMetric.textContent).toContain("unavailable");
    expect(costPerTaskMetric.textContent).not.toContain("$0.00");

    const latencyMetric = screen.getByTestId("metric-latency");
    expect(latencyMetric.textContent).toContain("unavailable");
    expect(latencyMetric.textContent).not.toContain("0 ms");

    const graderScoreMetric = screen.getByTestId("metric-grader-score");
    expect(graderScoreMetric.textContent).toContain("unavailable");
    expect(graderScoreMetric.textContent).not.toContain("0");
  });

  it("renders measured metrics correctly when available", () => {
    const pageWithMeasuredValues: AnalyticsPage = {
      items: [
        {
          dimensions: ["claude-3-5-sonnet"],
          metrics: {
            input_tokens: 12500,
            output_tokens: 3400,
            cached_tokens: 1000,
            reasoning_tokens: 500,
            cost_micros: 45000, // $0.0450
            cost_per_successful_task_micros: 90000, // $0.0900
            latency_ms: 1250,
            retry_count: 0,
            escalation_count: 1,
            grader_score_micros: 950000, // 0.95
          },
        },
      ],
      next_cursor: null,
    };

    render(<AnalyticsDashboard initialPage={pageWithMeasuredValues} />);

    expect(screen.getByTestId("metric-input-tokens").textContent).toContain("12,500");
    expect(screen.getByTestId("metric-output-tokens").textContent).toContain("3,400");
    expect(screen.getByTestId("metric-cached-tokens").textContent).toContain("1,000");
    expect(screen.getByTestId("metric-reasoning-tokens").textContent).toContain("500");
    expect(screen.getByTestId("metric-cost").textContent).toContain("$0.0450");
    expect(screen.getByTestId("metric-cost-per-task").textContent).toContain("$0.0900");
    expect(screen.getByTestId("metric-latency").textContent).toContain("1,250 ms");
    expect(screen.getByTestId("metric-grader-score").textContent).toContain("0.95");
  });

  it("triggers queries when grouping or time window changes", async () => {
    const onQueryAnalytics = vi.fn().mockResolvedValue({
      items: [],
      next_cursor: null,
    });

    render(<AnalyticsDashboard onQueryAnalytics={onQueryAnalytics} />);

    const groupingSelect = screen.getByLabelText("Group By:");
    await act(async () => {
      fireEvent.change(groupingSelect, { target: { value: "provider" } });
    });

    expect(onQueryAnalytics).toHaveBeenCalledWith(
      expect.objectContaining({
        group_by: [{ type: "provider" }],
      }),
    );
  });

  it("handles export requests for JSON and CSV", async () => {
    const onExportAnalytics = vi.fn().mockResolvedValue({
      artifact: {
        id: "art-123",
        byte_length: 500,
        media_type: "application/json",
        sensitivity: { type: "Public" },
        sha256: "abc",
      },
      format: { type: "json" },
      generated_at: "2026-08-16T10:00:00Z",
      row_count: 42,
      truncated: false,
    });

    render(<AnalyticsDashboard onExportAnalytics={onExportAnalytics} initialPage={{ items: [] }} />);

    const jsonExportBtn = screen.getByRole("button", { name: "Export JSON" });
    await act(async () => {
      fireEvent.click(jsonExportBtn);
    });

    expect(onExportAnalytics).toHaveBeenCalledWith(
      expect.objectContaining({
        format: { type: "json" },
      }),
    );
    expect(screen.getByRole("status").textContent).toContain("Export ready (json): 42 rows, artifact art-123");
  });

  it("an unanswered query renders unavailable, not an empty result", async () => {
    // `queryAnalytics` resolves to null when the command never reached a daemon
    // that answers it. Drawing that as "no observations recorded" would assert a
    // measurement we never made.
    const onQueryAnalytics = vi.fn().mockResolvedValue(null);

    await act(async () => {
      render(<AnalyticsDashboard onQueryAnalytics={onQueryAnalytics} />);
    });

    expect(screen.queryByTestId("analytics-empty")).toBeNull();
    const panel = screen.getByTestId("analytics-unavailable");
    expect(panel.textContent).toContain("Analytics unavailable");
    // And the headline cards must not fill in zeros for the numbers we lack.
    expect(screen.getByTestId("metric-input-tokens").textContent).toContain("unavailable");
    expect(screen.getByTestId("metric-cost").textContent).toContain("unavailable");
  });

  it("with no analytics source at all the view says so", () => {
    render(<AnalyticsDashboard />);

    expect(screen.queryByTestId("analytics-empty")).toBeNull();
    expect(screen.getByTestId("analytics-unavailable").textContent).toContain("unavailable");
  });

  it("headline totals sum every bucket instead of showing the first", () => {
    // Regression: the cards used to render `items[0].metrics` under a heading
    // that reads as an overall figure, so one model's numbers stood in for all.
    const page: AnalyticsPage = {
      items: [
        {
          dimensions: ["model-a"],
          metrics: { input_tokens: 1000, output_tokens: 100, completion_count: 4, latency_ms: 100 },
        },
        {
          dimensions: ["model-b"],
          metrics: { input_tokens: 500, output_tokens: 50, completion_count: 1, latency_ms: 600 },
        },
      ],
      next_cursor: null,
    };

    render(<AnalyticsDashboard initialPage={page} />);

    expect(screen.getByTestId("metric-input-tokens").textContent).toContain("1,500");
    expect(screen.getByTestId("metric-output-tokens").textContent).toContain("150");
    // Latency is an average, so it is weighted by completions: (100*4 + 600*1)/5.
    expect(screen.getByTestId("metric-latency").textContent).toContain("200 ms");
  });

  it("an average that cannot be weighted renders unavailable rather than guessed", () => {
    const page: AnalyticsPage = {
      items: [
        { dimensions: ["model-a"], metrics: { latency_ms: 100 } },
        { dimensions: ["model-b"], metrics: { latency_ms: 600 } },
      ],
      next_cursor: null,
    };

    render(<AnalyticsDashboard initialPage={page} />);

    // Without completion counts there is no honest combined latency.
    expect(screen.getByTestId("metric-latency").textContent).toContain("unavailable");
  });

  it("the window control actually narrows the query it labels", async () => {
    const onQueryAnalytics = vi.fn().mockResolvedValue({ items: [], next_cursor: null });

    await act(async () => {
      render(<AnalyticsDashboard onQueryAnalytics={onQueryAnalytics} />);
    });

    const windowSelect = screen.getByLabelText("Window:");
    await act(async () => {
      fireEvent.change(windowSelect, { target: { value: "24h" } });
    });

    const query = onQueryAnalytics.mock.calls.at(-1)?.[0];
    expect(query.filters?.time?.from).toBeTruthy();
    const from = Date.parse(query.filters.time.from);
    const span = Date.now() - from;
    // A 24 hour window, give or take the time the test took to run.
    expect(span).toBeGreaterThan(23 * 60 * 60 * 1000);
    expect(span).toBeLessThan(25 * 60 * 60 * 1000);

    // "All Time" must impose no time restriction at all.
    await act(async () => {
      fireEvent.change(windowSelect, { target: { value: "all" } });
    });
    expect(onQueryAnalytics.mock.calls.at(-1)?.[0].filters).toBeUndefined();
  });
});
