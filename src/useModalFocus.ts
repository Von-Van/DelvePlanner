import { RefObject, useEffect, useRef, useState } from "react";

const focusableSelector =
  'button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]';

/**
 * Keeps Tab focus inside a modal, closes it with Escape unless it is busy, and returns focus to
 * the element that was focused when the modal opened.
 */
export function useModalFocus(
  dialogRef: RefObject<HTMLElement>,
  onClose: () => void,
  busy: boolean,
) {
  // Captured during the first render, before `autoFocus` moves focus into the dialog.
  const [opener] = useState(() => document.activeElement as HTMLElement | null);
  const onCloseRef = useRef(onClose);
  const busyRef = useRef(busy);
  onCloseRef.current = onClose;
  busyRef.current = busy;

  useEffect(() => {
    const dialog = dialogRef.current;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busyRef.current) onCloseRef.current();
      if (event.key !== "Tab" || !dialog) return;
      const items = dialog.querySelectorAll<HTMLElement>(focusableSelector);
      if (!items.length) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      // Restore focus only once the dialog has really left the DOM; React's development-only
      // effect replay unmounts effects without removing the element.
      window.setTimeout(() => {
        if (!dialog?.isConnected) opener?.focus();
      });
    };
  }, [dialogRef, opener]);
}
