import {
  KeyboardEvent,
  ReactNode,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";

export type MenuItem =
  | {
      kind?: "item";
      label: string;
      onSelect: () => void;
      disabled?: boolean;
      danger?: boolean;
      hint?: string;
    }
  | { kind: "separator" };

/**
 * A button that opens a WAI-ARIA menu. Arrow keys, Home, and End move between items; Escape
 * closes and returns focus to the button. The list is fixed-positioned so scrolling containers
 * never clip it, and it closes when the page scrolls.
 */
export function Menu({
  label,
  trigger,
  items,
  className = "",
  disabled = false,
}: {
  label: string;
  trigger: ReactNode;
  items: MenuItem[];
  className?: string;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{
    top: number;
    left: number;
  } | null>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const id = useId();

  const close = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    setPosition(null);
    if (restoreFocus) buttonRef.current?.focus({ preventScroll: true });
  }, []);

  useLayoutEffect(() => {
    if (!open || !buttonRef.current || !menuRef.current) return;
    const trigger = buttonRef.current.getBoundingClientRect();
    const menu = menuRef.current.getBoundingClientRect();
    const margin = 8;
    let top = trigger.bottom + 4;
    if (top + menu.height > window.innerHeight - margin)
      top = Math.max(margin, trigger.top - menu.height - 4);
    const left = Math.min(
      Math.max(margin, trigger.right - menu.width),
      window.innerWidth - menu.width - margin,
    );
    setPosition({ top, left });
    enabledItems(menuRef.current)[0]?.focus({ preventScroll: true });
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onPointer = (event: MouseEvent) => {
      const target = event.target as Node;
      if (
        !menuRef.current?.contains(target) &&
        !buttonRef.current?.contains(target)
      )
        close(false);
    };
    const onViewportChange = () => close(false);
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("scroll", onViewportChange, true);
    window.addEventListener("resize", onViewportChange);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("scroll", onViewportChange, true);
      window.removeEventListener("resize", onViewportChange);
    };
  }, [open, close]);

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!menuRef.current) return;
    const options = enabledItems(menuRef.current);
    const index = options.indexOf(document.activeElement as HTMLButtonElement);
    const target = {
      ArrowDown: options[(index + 1) % options.length],
      ArrowUp: options[(index - 1 + options.length) % options.length],
      Home: options[0],
      End: options[options.length - 1],
    }[event.key];
    if (target) {
      event.preventDefault();
      target.focus({ preventScroll: true });
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
    } else if (event.key === "Tab") {
      close(false);
    }
  }

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        className={className}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        aria-label={label}
        title={label}
        disabled={disabled}
        onClick={() => (open ? close(false) : setOpen(true))}
      >
        {trigger}
      </button>
      {open &&
        createPortal(
          <div
            ref={menuRef}
            id={id}
            role="menu"
            aria-label={label}
            className="menu-popover"
            style={
              position
                ? { top: position.top, left: position.left }
                : { top: 0, left: 0, visibility: "hidden" }
            }
            onKeyDown={onKeyDown}
          >
            {items.map((item, index) =>
              item.kind === "separator" ? (
                <div
                  key={`separator-${index}`}
                  role="separator"
                  className="menu-separator"
                />
              ) : (
                <button
                  key={item.label}
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  className={`menu-item ${item.danger ? "danger" : ""}`}
                  disabled={item.disabled}
                  onClick={() => {
                    close(true);
                    item.onSelect();
                  }}
                >
                  <span>{item.label}</span>
                  {item.hint && <small>{item.hint}</small>}
                </button>
              ),
            )}
          </div>,
          document.body,
        )}
    </>
  );
}

function enabledItems(menu: HTMLElement) {
  return [
    ...menu.querySelectorAll<HTMLButtonElement>(
      '[role="menuitem"]:not(:disabled)',
    ),
  ];
}
