import { render } from "@testing-library/react";
import type { StreamName } from "@codypendent/control-plane";
import { describe, expect, expectTypeOf, it, vi } from "vitest";
import { useStream, type UseStreamOptions } from "../src/hooks/useStream.js";

const streamMocks = vi.hoisted(() => {
  const unsubscribe = vi.fn();
  return {
    subscribe: vi.fn(() => unsubscribe),
    unsubscribe,
  };
});

vi.mock("../src/hooks/useControlPlaneContext.js", () => ({
  useControlPlaneContext: () => ({
    activeOrganizationId: "org-1",
    streamClient: { subscribe: streamMocks.subscribe },
  }),
}));

function StreamProbe() {
  useStream({ stream: "policy" });
  return <div>stream probe</div>;
}

describe("useStream", () => {
  it("requires and forwards an explicit stream scope", () => {
    expectTypeOf<UseStreamOptions["stream"]>().toEqualTypeOf<StreamName>();

    const view = render(<StreamProbe />);
    expect(streamMocks.subscribe).toHaveBeenCalledWith(
      expect.objectContaining({ organizationId: "org-1", stream: "policy" })
    );

    view.unmount();
    expect(streamMocks.unsubscribe).toHaveBeenCalledTimes(1);
  });
});
