import { describe, expect, it } from "vitest";
import { Button, Stack } from "../src/index.js";
import { renderForTest } from "../src/testing.js";

describe("UiTestRenderer", () => {
  it("serializes deterministically and creates revision-bound events", () => {
    const renderer = renderForTest(Stack({ id: "root", children: Button({ id: "run", action: "run", label: "Run" }) }));
    expect(renderer.toJSON()).toContain('"documentId": "test-document"');
    expect(renderer.dispatch("run", "action")).toMatchObject({ eventId: "test-event-1", revision: 0, targetId: "run" });
  });
});
