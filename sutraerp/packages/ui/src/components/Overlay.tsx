import { useEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";

/**
 * Shared overlay behaviour: portal, backdrop click + Escape to close, focus restore.
 * The initial focus + restore run once per opening, and the initial focus never moves a focus
 * that is already inside the panel: `onClose` is read through a ref, so a parent
 * re-render (a fresh `onClose` arrow, e.g. after a background refetch) never pulls focus back to
 * the first field while the user is typing in another one.
 */
export function useOverlay(open: boolean, onClose: () => void) {
  const panel = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement as HTMLElement | null;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close.current();
    };
    document.addEventListener("keydown", onKey);
    // Move focus into the panel unless the user (or a test driver) already put it there.
    const t = setTimeout(() => {
      const p = panel.current;
      if (p && !p.contains(document.activeElement)) p.querySelector<HTMLElement>("input,button,[tabindex]")?.focus();
    }, 30);
    return () => {
      clearTimeout(t);
      document.removeEventListener("keydown", onKey);
      previous?.focus?.();
    };
  }, [open]);
  return panel;
}

export function Portal({ children }: { children: ReactNode }) {
  if (typeof document === "undefined") return null;
  return createPortal(children, document.body);
}
