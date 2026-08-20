/**
 * A view fed an inline loader must not re-fetch on every app render.
 *
 * Every one of these components is given its loader as a prop from `App.tsx`,
 * written there as an inline arrow — a new function on each render. With the
 * loader in the effect's dependency array, that re-ran the fetch each time, and
 * several of those loaders call `setState` on the app while they run: render →
 * effect → fetch → app setState → render, as fast as the machine allows. On
 * screen it was the councils and repository pages flickering.
 */
import { render, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useState } from "react";

import { CouncilBrowser } from "../src/components/CouncilBrowser.js";
import { RepoPicker } from "../src/components/RepoPicker.js";

describe("loaders are not re-run by their own re-render", () => {
  it("loads a council list once, even when the parent re-renders and the prop is inline", async () => {
    let loads = 0;
    // A parent that re-renders whenever the child's load resolves — exactly
    // what `App.tsx` does by calling `setCouncilNames` inside `onLoad`.
    function Parent() {
      const [, setTick] = useState(0);
      return (
        <CouncilBrowser
          onLoad={async () => {
            loads += 1;
            setTick((value) => value + 1);
            return [];
          }}
        />
      );
    }
    render(<Parent />);
    await waitFor(() => expect(loads).toBeGreaterThan(0));
    // Give any runaway loop a generous chance to show itself.
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(loads).toBe(1);
  });

  it("loads the repository list once under the same conditions", async () => {
    let loads = 0;
    function Parent() {
      const [, setTick] = useState(0);
      return (
        <RepoPicker
          onLoad={async () => {
            loads += 1;
            setTick((value) => value + 1);
            return { current: null, recent: [] } as never;
          }}
          onPick={async () => null}
        />
      );
    }
    render(<Parent />);
    await waitFor(() => expect(loads).toBeGreaterThan(0));
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(loads).toBe(1);
  });
});
