/**
 * Command palette (prototype `.pal`, Ctrl K): grouped results (Suggested · Actions · Screens ·
 * Workspaces), the matched part of each title highlighted, and a keyboard footer.
 * Local to the shell (R1); the `@sutra/ui` CommandPalette has no highlight / footer yet (NEEDS-R0).
 */
import { Search } from "lucide-react";
import { IconChip, Modal, type PaletteItem } from "@sutra/ui";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";

export interface PaletteEntry extends PaletteItem {
  /** Extra words that match (not shown). */
  keywords?: string;
  /** Keyboard shortcut shown at the end of the row. */
  kbd?: string;
}

function highlight(text: string, q: string): ReactNode {
  if (!q) return text;
  const i = text.toLowerCase().indexOf(q);
  if (i < 0) return text;
  return (
    <>
      {text.slice(0, i)}
      <mark>{text.slice(i, i + q.length)}</mark>
      {text.slice(i + q.length)}
    </>
  );
}

export function Palette({ open, onClose, items, suggested, onSelect, limit = 40 }: { open: boolean; onClose: () => void; items: PaletteEntry[]; suggested: PaletteEntry[]; onSelect: (item: PaletteEntry) => void; limit?: number }) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (open) {
      setQuery("");
      setSelected(0);
    }
  }, [open]);
  const q = query.trim().toLowerCase();
  const shown = useMemo(() => {
    if (!q) return suggested.map((x) => ({ ...x, group: "Suggested" }));
    const score = (x: PaletteEntry) => {
      const t = x.title.toLowerCase();
      return t.startsWith(q) ? 0 : t.includes(q) ? 1 : 2;
    };
    return items
      .filter((x) => `${x.title} ${x.subtitle} ${x.keywords ?? ""}`.toLowerCase().includes(q))
      .sort((a, b) => GROUP_ORDER.indexOf(a.group) - GROUP_ORDER.indexOf(b.group) || score(a) - score(b))
      .slice(0, limit);
  }, [items, suggested, q, limit]);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
  }, [selected]);
  const pick = (i: number) => {
    const item = shown[i];
    if (item) {
      onClose();
      onSelect(item);
    }
  };
  let lastGroup = "";
  return (
    <Modal open={open} onClose={onClose} label="Command palette" padded={false}>
      <div className="sb-palette-input">
        <Search strokeWidth={1.75} aria-hidden="true" />
        <input
          value={query}
          placeholder="Search or type a command"
          aria-label="Command palette"
          data-palette-input
          onChange={(e) => {
            setQuery(e.target.value);
            setSelected(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              setSelected((s) => Math.min(s + 1, shown.length - 1));
              e.preventDefault();
            } else if (e.key === "ArrowUp") {
              setSelected((s) => Math.max(s - 1, 0));
              e.preventDefault();
            } else if (e.key === "Enter") {
              pick(selected);
            }
          }}
        />
        <kbd className="sb-kbd">Esc</kbd>
      </div>
      <div className="sb-palette-list" role="listbox" ref={list}>
        {shown.length === 0 && <p className="sb-palette-group">No matches for “{query.trim()}”</p>}
        {shown.map((x, i) => {
          const header = x.group !== lastGroup ? <p className="sb-palette-group">{x.group}</p> : null;
          lastGroup = x.group;
          return (
            <div key={`${x.group}:${x.id}`}>
              {header}
              <button type="button" role="option" aria-selected={i === selected} className={`sb-pop-item pal-row${i === selected ? " is-selected" : ""}`} onMouseMove={() => setSelected(i)} onClick={() => pick(i)} data-palette-item={x.id}>
                <IconChip icon={x.icon} tone={x.tone} size="sm" />
                <div>
                  <b>{highlight(x.title, q)}</b>
                  <small>{x.subtitle}</small>
                </div>
                {x.kbd && <kbd className="sb-kbd">{x.kbd}</kbd>}
              </button>
            </div>
          );
        })}
      </div>
      <div className="pal-foot" aria-hidden="true">
        <span>
          <kbd className="sb-kbd">↑</kbd>
          <kbd className="sb-kbd">↓</kbd> move
        </span>
        <span>
          <kbd className="sb-kbd">Enter</kbd> run
        </span>
        <span>
          <kbd className="sb-kbd">?</kbd> all shortcuts
        </span>
      </div>
    </Modal>
  );
}

const GROUP_ORDER = ["Suggested", "Actions", "Screens", "Workspaces", "Display"];
