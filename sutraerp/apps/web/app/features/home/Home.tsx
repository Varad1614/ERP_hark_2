/**
 * Customizable Home (re-skin v2, prototype dash.js).
 *
 * The layout comes from the user's own copy (localStorage per user + role), else a default an
 * administrator published on this device, else the built-in layout of the role. Customize mode
 * (avatar menu → Personalization → Customize Home, or E) lets the user drag tiles to reorder,
 * drag the corner grip or Shift+arrows to resize, add / remove widgets and colour tiles; every
 * edit saves itself and Undo walks back. Focus mode (F) keeps only Right now + To-dos.
 */
import { Check, Focus, GripVertical, LayoutDashboard, LayoutTemplate, Palette, Plus, RotateCcw, Send, SwatchBook, Undo2, X } from "lucide-react";
import { useToast } from "@sutra/ui";
import { useCallback, useEffect, useRef, useState, type CSSProperties, type DragEvent, type KeyboardEvent, type PointerEvent } from "react";
import { useSession } from "../../lib/auth";
import { HomePanels, TilePopover } from "./Customize";
import { focusLayout, keyEdit, moveTile, removeTile, resizeTile, tileLabel, tileTone, updateTile, WIDGETS, type HomeLayout, type HomeTile } from "./model";
import { bindLayout, effectiveLayout, homeUi, setLayout, useHomeUi, useLayoutState, useTheme } from "./store";
import { WIDGET_COMPONENTS } from "./widgets";
import "./home.css";


export default function Home() {
  const { role, loginId } = useSession();
  bindLayout(`${loginId ?? role}.${role}`, role);
  const ls = useLayoutState();
  const layout = effectiveLayout(ls);
  const { editing, focus } = useHomeUi();
  const theme = useTheme();
  const toast = useToast();
  const label = (t: HomeTile) => tileLabel(t, role);
  const [history, setHistory] = useState<HomeLayout[]>([]);
  const [popFor, setPopFor] = useState<string | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const gridRef = useRef<HTMLDivElement>(null);

  // Leaving Home ends customize and focus mode.
  useEffect(
    () => () => {
      homeUi.stopCustomize();
      homeUi.setFocus(false);
    },
    [],
  );
  useEffect(() => {
    if (!editing) setPopFor(null);
  }, [editing]);
  useEffect(() => {
    if (!focus) return;
    const onKey = (e: globalThis.KeyboardEvent) => e.key === "Escape" && homeUi.setFocus(false);
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [focus]);

  const shown = focus ? focusLayout() : layout;

  const commit = useCallback(
    (next: HomeLayout, focusId?: string) => {
      setHistory((h) => [...h.slice(-39), layout]);
      setLayout(next);
      if (focusId) requestAnimationFrame(() => (gridRef.current?.querySelector(`[data-tile="${focusId}"]`) as HTMLElement | null)?.focus({ preventScroll: false }));
    },
    [layout],
  );
  const undo = () => {
    const prev = history[history.length - 1];
    if (!prev) return;
    setHistory((h) => h.slice(0, -1));
    setLayout(prev);
    toast("Undone");
  };
  const reset = () => {
    setHistory((h) => [...h.slice(-39), layout]);
    setLayout(undefined);
    toast("Home reset to the standard layout");
  };

  const onKey = (e: KeyboardEvent<HTMLDivElement>, t: HomeTile) => {
    if (!editing || e.target !== e.currentTarget) return;
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      commit(removeTile(layout, t.id));
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      setPopFor(t.id);
      return;
    }
    const next = keyEdit(layout, t.id, e.key, e.shiftKey);
    if (next) {
      e.preventDefault();
      if (next !== layout) commit(next, t.id);
    }
  };

  /* ---- drag to reorder (HTML5 drag and drop) ---- */
  const onDragStart = (e: DragEvent, t: HomeTile) => {
    setDragId(t.id);
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", t.id);
  };
  const onDrop = (e: DragEvent, target: HomeTile) => {
    e.preventDefault();
    const id = dragId ?? e.dataTransfer.getData("text/plain");
    setDragId(null);
    if (!id || id === target.id) return;
    commit(moveTile(layout, id, layout.items.findIndex((x) => x.id === target.id)));
  };

  /* ---- corner grip resize (pointer) ---- */
  const startResize = (e: PointerEvent<HTMLSpanElement>, t: HomeTile) => {
    e.preventDefault();
    e.stopPropagation();
    const grid = gridRef.current;
    if (!grid) return;
    const col = (grid.clientWidth + theme.gap) / 12;
    const row = 140;
    const x0 = e.clientX;
    const y0 = e.clientY;
    const before = layout;
    let last = before;
    const move = (ev: globalThis.PointerEvent) => {
      last = resizeTile(before, t.id, { w: t.w + Math.round((ev.clientX - x0) / col), h: t.h + Math.round((ev.clientY - y0) / row) });
      setLayout(last);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      document.body.classList.remove("drsz");
      if (last !== before) setHistory((h) => [...h.slice(-39), before]);
    };
    document.body.classList.add("drsz");
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  const onTileText = useCallback((id: string, text: string) => setLayout(updateTile(effectiveLayout(), id, { text: text || undefined })), []);
  const popTile = editing && popFor ? layout.items.find((x) => x.id === popFor) : undefined;

  return (
    <div className={`dwrap${focus ? " is-focus" : ""}`} data-home={role} data-home-focus={focus || undefined} data-home-editing={editing || undefined}>
      {editing && (
        <div className="dbar ed" role="toolbar" aria-label="Customize your Home" data-home-customize>
          <div className="dmsg">
            <b>Customizing your Home</b>
            <small>Drag tiles to move · drag a corner to resize · everything saves automatically</small>
          </div>
          <button type="button" className="dbtn" onClick={() => homeUi.openPanel("library")} data-home-add>
            <Plus strokeWidth={1.75} /> Add widget
          </button>
          <button type="button" className="dbtn" onClick={() => homeUi.openPanel("layouts")} data-home-layouts>
            <LayoutTemplate strokeWidth={1.75} /> Layouts
          </button>
          <button type="button" className="dbtn" onClick={() => homeUi.openPanel("theme")} data-home-theme>
            <SwatchBook strokeWidth={1.75} /> Theme
          </button>
          <button type="button" className="dbtn" onClick={undo} disabled={history.length === 0} data-home-undo>
            <Undo2 strokeWidth={1.75} /> Undo
          </button>
          <button type="button" className="dbtn" onClick={reset} disabled={!ls.own} data-home-reset>
            <RotateCcw strokeWidth={1.75} /> Reset
          </button>
          {role === "administrator" && (
            <button type="button" className="dbtn" onClick={() => homeUi.openPanel("publish")} data-home-publish>
              <Send strokeWidth={1.75} /> Publish as default
            </button>
          )}
          <button type="button" className="dbtn pri" onClick={() => homeUi.stopCustomize()} data-home-done>
            <Check strokeWidth={1.75} /> Done
          </button>
        </div>
      )}
      {focus && (
        <div className="dbar ed dfbar" role="status" data-focus-bar>
          <Focus strokeWidth={1.75} className="dfbar-ic" />
          <div className="dmsg">
            <b>Focus</b>
            <small>Just your queue · Esc to exit</small>
          </div>
          <button type="button" className="dbtn" onClick={() => homeUi.setFocus(false)} data-focus-exit>
            <X strokeWidth={1.75} /> Exit
          </button>
        </div>
      )}
      <div ref={gridRef} className={`dg${editing ? " ed" : ""}${theme.dense ? " dense" : ""}`} data-home-grid={shown.items.length}>
        {shown.items.length === 0 ? (
          <div className="dempty sb-tile">
            <LayoutDashboard strokeWidth={1.75} />
            <p>Your Home is empty. Add widgets to build it your way.</p>
            <button
              type="button"
              className="sb-btn"
              onClick={() => {
                homeUi.startCustomize();
                homeUi.openPanel("library");
              }}
            >
              <Plus strokeWidth={1.75} /> Add widget
            </button>
          </div>
        ) : (
          shown.items.map((t) => {
            const W = WIDGET_COMPONENTS[t.type];
            const tone = tileTone(t.color ?? WIDGETS[t.type].color);
            const style = { "--w": t.w, "--h": t.h, ...(tone.background ? { "--tile-bg": tone.background } : {}) } as CSSProperties;
            return (
              <div
                key={t.id}
                className={`dw${t.w <= 4 ? " sm" : ""}${tone.background ? (tone.dark ? " tinted tc-dark" : " tinted tc-light") : ""}${dragId === t.id ? " dragging" : ""}`}
                style={style}
                data-tile={t.id}
                data-type={t.type}
                draggable={editing}
                onDragStart={editing ? (e) => onDragStart(e, t) : undefined}
                onDragOver={editing ? (e) => e.preventDefault() : undefined}
                onDrop={editing ? (e) => onDrop(e, t) : undefined}
                onDragEnd={() => setDragId(null)}
                onKeyDown={(e) => onKey(e, t)}
                {...(editing
                  ? { tabIndex: 0, role: "group", "aria-roledescription": "movable widget", "aria-label": `${label(t)}, ${t.w} columns by ${t.h} rows. Arrow keys move, Shift plus arrows resize, Delete removes, Enter customizes.` }
                  : {})}
              >
                <div className="dbody" inert={editing || undefined}>
                  <W tile={t} editing={editing} onTileText={onTileText} />
                </div>
                {editing && (
                  <>
                    <div className="dlab" aria-hidden="true">
                      <GripVertical strokeWidth={1.75} />
                      {label(t)} <em>{`${t.w}×${t.h}`}</em>
                    </div>
                    <div className="dctl">
                      <button type="button" aria-label={`Customize ${label(t)}`} title="Colour & size" onClick={() => setPopFor(popFor === t.id ? null : t.id)} data-tile-settings={t.id}>
                        <Palette strokeWidth={1.75} />
                      </button>
                      <button type="button" aria-label={`Remove ${label(t)}`} title="Remove" onClick={() => commit(removeTile(layout, t.id))} data-tile-remove={t.id}>
                        <X strokeWidth={1.75} />
                      </button>
                    </div>
                    <span className="dgrip" title="Drag to resize" aria-hidden="true" onPointerDown={(e) => startResize(e, t)} />
                    {popTile?.id === t.id && <TilePopover tile={popTile} layout={layout} commit={commit} onClose={() => setPopFor(null)} />}
                  </>
                )}
              </div>
            );
          })
        )}
      </div>
      <HomePanels layout={layout} commit={commit} />
    </div>
  );
}
