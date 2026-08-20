import { useEffect, useRef } from "react";

/**
 * Run `load` once, when the component mounts, whatever happens to its identity.
 *
 * These views are all fed their loader as a prop from `App.tsx`, where it is
 * written as an inline arrow — so it is a NEW function on every render of the
 * app. A plain `useEffect(() => void load(), [load])` therefore re-ran on every
 * app render, and several of those loaders call `setState` on the app while
 * they run. That closes the loop: render → effect → fetch → app setState →
 * render. On screen it is a page flickering as fast as it can re-fetch, which
 * is exactly what the councils and repository pickers were doing.
 *
 * Holding the callback in a ref keeps the mount load correct without making it
 * hostage to caller memoization — a call site that forgets `useCallback` costs
 * nothing here, rather than melting the view. Explicit refresh still calls the
 * loader directly, so a Refresh button is unaffected.
 */
export function useLoadOnMount(load: () => void | Promise<void>): void {
  const latest = useRef(load);
  latest.current = load;
  useEffect(() => {
    void latest.current();
  }, []);
}
