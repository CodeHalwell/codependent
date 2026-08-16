import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App.js";

describe("desktop without daemon transport", () => {
  afterEach(() => vi.useRealTimers());

  it("reports an honest unavailable daemon integration state", () => {
    render(<App />);

    expect(screen.getByText("codypendentd: disconnected")).toBeTruthy();
    expect(screen.getByText("Daemon integration is not available in this build.")).toBeTruthy();
    expect(screen.getByPlaceholderText("Run controls are unavailable until a transport-enabled build.")).toBeTruthy();
    expect(screen.getByText("Controls unavailable")).toBeTruthy();
    expect(screen.queryByText(/connect to codypendentd to start a run/i)).toBeNull();
    expect(screen.queryByText(/build mode.*active scope/i)).toBeNull();
    expect(screen.queryByText(/start codypendentd to make it available/i)).toBeNull();
  });

  it("does not create sessions or runs as time elapses", () => {
    vi.useFakeTimers();
    render(<App />);

    expect(screen.getByText("No sessions yet")).toBeTruthy();
    expect(screen.queryByText(/executing task/i)).toBeNull();

    act(() => vi.advanceTimersByTime(60_000));

    expect(screen.getByText("No sessions yet")).toBeTruthy();
    expect(screen.queryByText(/executing task/i)).toBeNull();
  });

  it("disables session and run controls", () => {
    render(<App />);

    const newSession = screen.getByRole("button", { name: /new/i }) as HTMLButtonElement;
    const objective = screen.getByRole("textbox") as HTMLTextAreaElement;
    const send = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(newSession.disabled).toBe(true);
    expect(objective.disabled).toBe(true);
    expect(send.disabled).toBe(true);

    fireEvent.click(newSession);
    expect(screen.getByText("No sessions yet")).toBeTruthy();
  });
});
