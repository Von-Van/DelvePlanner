import { RefObject, useEffect, useRef } from "react";

/**
 * Moves focus to a view's heading after the user navigates to it, so keyboard and screen-reader
 * users land on the new content. `token` changes on each navigation; 0 means the initial load.
 */
export function useHeadingFocus(
  headingRef: RefObject<HTMLElement>,
  token: number,
  ready = true,
) {
  const handled = useRef(0);
  useEffect(() => {
    if (!ready || token === 0 || handled.current === token) return;
    if (!headingRef.current) return;
    handled.current = token;
    headingRef.current.focus();
  }, [headingRef, token, ready]);
}
