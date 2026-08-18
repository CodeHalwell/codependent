// Measurement only: full-App cost of one streamed token, which is what the
// operator actually feels. Not part of the gate.
import { act, render } from "@testing-library/react";
import { describe, it } from "vitest";
import { App } from "../src/App.js";
import type { ConnectionInfo, DaemonFrame, DesktopTransport, SessionRow } from "../src/transport.js";

class Stub implements DesktopTransport {
  private frames: ((f: DaemonFrame) => void) | null = null;
  socketPath() { return Promise.resolve("/tmp/s.sock"); }
  connect(onFrame: (f: DaemonFrame) => void): Promise<ConnectionInfo> {
    this.frames = onFrame;
    return Promise.resolve({ socket_path: "/tmp/s.sock", protocol_version: "1.4",
      daemon_version: "0.12.1", daemon_instance: "i", build_id: "b" });
  }
  disconnect() { return Promise.resolve(); }
  listSessions(): Promise<SessionRow[]> { return Promise.resolve([]); }
  emit(f: DaemonFrame) { this.frames?.(f); }
}

function delta(sequence: number, text: string): DaemonFrame {
  return { kind: "event", session_id: "session-1",
    event: { sequence, actor: { type: "System" }, occurred_at: "2026-08-16T10:00:00Z",
      body: { type: "ModelStreamDelta", run_id: "run-1", text } as never } } as DaemonFrame;
}

describe("full app cost per streamed token", () => {
  for (const history of [50, 400]) {
    it(`history=${history}`, async () => {
      const t = new Stub();
      render(<App makeTransport={() => t as unknown as DesktopTransport} />);
      await act(async () => undefined);
      let seq = 1;
      // Build the back-history the operator would already have on screen.
      await act(async () => {
        for (let i = 0; i < history; i++) {
          t.emit({ kind: "event", session_id: "session-1",
            event: { sequence: seq++, actor: { type: "System" }, occurred_at: "2026-08-16T10:00:00Z",
              body: { type: "RunStarted", run_id: `old-${i}`,
                      objective: `an earlier turn of sentence length, number ${i}` } as never } } as DaemonFrame);
        }
      });
      const TOKENS = 300;
      const t0 = performance.now();
      for (let i = 0; i < TOKENS; i++) {
        await act(async () => { t.emit(delta(seq++, "token ")); });
      }
      const ms = performance.now() - t0;
      console.log(`  history=${String(history).padStart(4)}  ${(ms / TOKENS).toFixed(2)} ms per token  (${ms.toFixed(0)} ms / ${TOKENS})`);
    });
  }
});
