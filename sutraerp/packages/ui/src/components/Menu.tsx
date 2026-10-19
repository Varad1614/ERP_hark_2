import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { cx } from "./cx";

export interface MenuItem {
  id: string;
  label: string;
  /** Click handler; omit with `href`/`render` for links. */
  onSelect?: () => void;
  /** Destructive actions are tinted (Suspend, Cancel, Decommission). */
  danger?: boolean;
  disabled?: boolean;
  /** Small right-aligned hint (count, shortcut). */
  hint?: string;
  /** Marks the current item (e.g. the active overflow tab). */
  active?: boolean;
  /** Custom element instead of a button (a router link); receives the item className + close(). */
  render?: (props: { className: string; close: () => void }) => ReactNode;
  /** Separator before this item. */
  separated?: boolean;
  /** Group label shown before this item. */
  group?: string;
}

/** Open/close state of a menu anchored to its trigger: outside click and Escape close it. */
export function useMenu() {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const close = useCallback(() => setOpen(false), []);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);
  return { open, setOpen, close, ref };
}

/** The floating list of a menu (`.sb-menu`). */
export function MenuList({ items, close, label, align = "right", id }: { items: MenuItem[]; close: () => void; label: string; align?: "left" | "right"; id?: string }) {
  return (
    <div className={cx("sb-menu", align === "left" && "sb-menu--left")} role="menu" aria-label={label} id={id}>
      {items.map((it) => (
        <MenuEntry key={it.id} item={it} close={close} />
      ))}
    </div>
  );
}

function MenuEntry({ item: it, close }: { item: MenuItem; close: () => void }) {
  const className = cx(it.danger && "is-danger", it.active && "on");
  return (
    <>
      {it.separated && <div className="sb-menu-sep" role="separator" />}
      {it.group && <div className="sb-menu-label">{it.group}</div>}
      {it.render ? (
        it.render({ className, close })
      ) : (
        <button
          type="button"
          role="menuitem"
          className={className || undefined}
          disabled={it.disabled}
          data-menu-item={it.id}
          onClick={() => {
            close();
            it.onSelect?.();
          }}
        >
          {it.label}
          {it.hint && <small>{it.hint}</small>}
        </button>
      )}
    </>
  );
}

/** A trigger + menu pair; `trigger` receives the toggle props to spread on a button. */
export function MenuButton({
  label,
  items,
  align = "right",
  trigger,
  className,
}: {
  label: string;
  items: MenuItem[];
  align?: "left" | "right";
  trigger: (props: { onClick: () => void; "aria-expanded": boolean; "aria-haspopup": "menu"; "aria-controls": string }) => ReactNode;
  className?: string;
}) {
  const { open, setOpen, close, ref } = useMenu();
  const id = useId();
  return (
    <div ref={ref} className={cx("sb-rowmenu", className)}>
      {trigger({ onClick: () => setOpen((o) => !o), "aria-expanded": open, "aria-haspopup": "menu", "aria-controls": id })}
      {open && <MenuList items={items} close={close} label={label} align={align} id={id} />}
    </div>
  );
}
