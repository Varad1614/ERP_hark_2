/**
 * Customize panels of the Home (prototype dash.js): widget library, season layouts, the personal
 * theme drawer, "publish as default" and the per-tile colour / size popover. Opened from the
 * avatar menu's Personalization group, the customize bar, the palette or keyboard (E / F).
 */
import { Award, CalendarCheck, Check, House, LayoutTemplate, Plus, RotateCcw, Send, SwatchBook, UserPlus, X, type LucideIcon } from "lucide-react";
import { Badge, Button, Drawer, IconChip, ROLE_ACCENTS, useToast } from "@sutra/ui";
import { useState } from "react";
import { useSession } from "../../lib/auth";
import { ROLE_CONFIG, ROLES, type Role } from "../../lib/roles";
import { addTile, hasTile, KPI_NAMES, presetLayout, PRESETS, SWATCHES, tileTone, updateTile, resizeTile, WIDGET_TYPES, WIDGETS, WIDTHS, type HomeLayout, type HomeTile } from "./model";
import { ACCENTS, BACKGROUNDS, homeUi, publishDefault, setTheme, useHomeUi, useTheme, type ThemePrefs } from "./store";
import { WIDGET_ICONS } from "./widgets";


const PRESET_ICON: Record<string, LucideIcon> = { everyday: House, exam: CalendarCheck, admissions: UserPlus, results: Award, grading: Award, start: CalendarCheck };

function Seg<T extends string | number>({ name, value, options, onChange }: { name: string; value: T; options: ReadonlyArray<readonly [T, string]>; onChange: (v: T) => void }) {
  return (
    <div className="dseg" role="radiogroup" aria-label={name}>
      {options.map(([v, l]) => (
        <button key={String(v)} type="button" role="radio" aria-checked={v === value} className={v === value ? "on" : ""} onClick={() => onChange(v)} data-theme-opt={`${name.toLowerCase().replace(/\s+/g, "-")}:${v}`} {...displayHook(name, v)}>
          {l}
        </button>
      ))}
    </div>
  );
}

/** Keeps the pre-re-skin `data-display` hooks (e2e suites switch theme / contrast / font through them). */
function displayHook(name: string, v: string | number): Record<string, string> {
  if (name === "Mode" && (v === "light" || v === "dark" || v === "auto")) return { "data-display": `theme:${v}` };
  if (name === "Contrast") return { "data-display": `contrast:${v === "1" ? "high" : "standard"}` };
  if (name === "Font" && (v === "sutra" || v === "easy")) return { "data-display": `font:${v}` };
  return {};
}

export function ThemeDrawer({ open, onClose }: { open: boolean; onClose: () => void }) {
  const t = useTheme();
  const { role } = useSession();
  const toast = useToast();
  const set = (patch: Partial<ThemePrefs>) => setTheme(patch);
  return (
    <Drawer open={open} onClose={onClose} title="Theme" eyebrow="Personal · only you see this" media={<IconChip icon={SwatchBook} tone="violet" />}>
      <div className="dh-theme" data-theme-drawer>
        <div className="dprev" aria-hidden="true">
          <i />
          <i />
          <i />
          <i />
        </div>
        <div className="dsec">
          <p>Mode</p>
          <Seg name="Mode" value={t.mode} options={[["light", "Light"], ["dark", "Dark"], ["auto", "Auto"], ["oled", "OLED"]] as const} onChange={(mode) => set({ mode })} />
          {t.mode !== "light" && t.mode !== "dark" && <small className="dhint">{t.mode === "auto" ? "Follows your device setting." : "True black. Easier on the eyes at night."}</small>}
        </div>
        <div className="dsec">
          <p>Accent colour</p>
          <div className="dsw">
            <button type="button" className={t.accent ? "" : "on"} style={{ background: ROLE_ACCENTS[role].ac }} title="Role default" aria-label="Role default" onClick={() => set({ accent: undefined })} />
            {ACCENTS.map((c) => (
              <button key={c} type="button" className={t.accent === c ? "on" : ""} style={{ background: c }} aria-label={`Accent ${c}`} onClick={() => set({ accent: c })} />
            ))}
            <label title="Custom colour" aria-label="Custom accent">
              <input type="color" value={t.accent ?? ROLE_ACCENTS[role].ac} onChange={(e) => set({ accent: e.target.value.toUpperCase() })} />
            </label>
          </div>
        </div>
        <div className="dsec">
          <p>Background</p>
          <div className="dsw">
            {BACKGROUNDS.map(([c, name]) => {
              const v = c === "#FAF8F4" ? undefined : c;
              return <button key={c} type="button" className={t.bg === v ? "on" : ""} style={{ background: c }} title={name} aria-label={name} onClick={() => set({ bg: v })} />;
            })}
            <label title="Custom colour" aria-label="Custom background">
              <input type="color" value={t.bg ?? "#FAF8F4"} onChange={(e) => set({ bg: e.target.value.toUpperCase() })} />
            </label>
          </div>
        </div>
        <div className="dsec">
          <p>Corner roundness</p>
          <div className="drng">
            <input type="range" min={4} max={36} step={2} value={t.radius} aria-label="Corner roundness" onChange={(e) => set({ radius: Number(e.target.value) })} />
            <output>{t.radius}px</output>
          </div>
        </div>
        <div className="dsec">
          <p>Spacing</p>
          <Seg name="Spacing" value={t.gap} options={[[10, "Compact"], [16, "Comfortable"], [24, "Spacious"]] as const} onChange={(gap) => set({ gap })} />
        </div>
        <div className="dsec">
          <p>Layout</p>
          <Seg name="Layout" value={t.dense ? "1" : ""} options={[["", "Keep my order"], ["1", "Fill gaps automatically"]] as const} onChange={(v) => set({ dense: v === "1" })} />
        </div>
        <div className="dsec">
          <p>Tile depth</p>
          <Seg name="Tile depth" value={t.shadow} options={[["flat", "Flat"], ["soft", "Soft"], ["raised", "Raised"]] as const} onChange={(shadow) => set({ shadow })} />
        </div>
        <div className="dsec">
          <p>Font</p>
          <Seg name="Font" value={t.font} options={[["sutra", "Sutra"], ["system", "System"], ["serif", "Serif"], ["easy", "Easy read"]] as const} onChange={(font) => set({ font })} />
          {t.font === "easy" && <small className="dhint">Atkinson Hyperlegible, with wider letter and line spacing. Designed for low vision and dyslexia.</small>}
        </div>
        <div className="dsec">
          <p>Contrast</p>
          <Seg name="Contrast" value={t.hc ? "1" : ""} options={[["", "Standard"], ["1", "High contrast"]] as const} onChange={(v) => set({ hc: v === "1" })} />
        </div>
        <div className="dsec">
          <p>Text size</p>
          <Seg name="Text size" value={t.scale} options={[[0.9, "Small"], [1, "Default"], [1.1, "Large"]] as const} onChange={(scale) => set({ scale })} />
        </div>
        <div className="sb-drawer-footer">
          <Button
            variant="ghost"
            icon={<RotateCcw strokeWidth={1.75} />}
            iconPosition="start"
            onClick={() => {
              setTheme(null);
              toast("Theme reset");
            }}
            data-theme-reset
          >
            Reset theme
          </Button>
          <Button onClick={onClose} data-theme-done>
            Done
          </Button>
        </div>
      </div>
    </Drawer>
  );
}

export function LayoutsDrawer({ open, onClose, layout, commit }: { open: boolean; onClose: () => void; layout: HomeLayout; commit: (next: HomeLayout) => void }) {
  const { role } = useSession();
  const toast = useToast();
  const current = layout.preset ?? "everyday";
  return (
    <Drawer open={open} onClose={onClose} title="Switch your Home" eyebrow="Layouts for the season" media={<IconChip icon={LayoutTemplate} tone="teal" />}>
      <p className="dh-lead">Swap the whole layout in one tap. Your sticky notes come along, and Undo brings the old layout back.</p>
      <div className="dh-presets">
        {PRESETS[role].map(([key, name, desc]) => {
          const Icon = PRESET_ICON[key] ?? House;
          const on = key === current;
          return (
            <button
              key={key}
              type="button"
              className={`sb-row${on ? " is-on" : ""}`}
              data-preset={key}
              onClick={() => {
                if (!on) {
                  commit(presetLayout(key, layout, role));
                  toast(`Layout: ${name}`);
                }
                onClose();
              }}
            >
              <IconChip icon={Icon} tone={on ? "green" : "grey"} />
              <div>
                <b>{name}</b>
                <small>{desc}</small>
              </div>
              {on ? <Badge kind="done">Current</Badge> : <span className="dh-chev" aria-hidden="true">›</span>}
            </button>
          );
        })}
      </div>
    </Drawer>
  );
}

export function LibraryDrawer({ open, onClose, layout, commit }: { open: boolean; onClose: () => void; layout: HomeLayout; commit: (next: HomeLayout, focusId?: string) => void }) {
  const { role } = useSession();
  const toast = useToast();
  const types = WIDGET_TYPES.filter((t) => t !== "kpi" && (!WIDGETS[t].roles || WIDGETS[t].roles!.includes(role)));
  const add = (type: HomeTile["type"], i?: number) => {
    const { layout: next, tile } = addTile(layout, type, i);
    commit(next, tile.id);
    toast(`Added: ${i === undefined ? WIDGETS[type].name : KPI_NAMES[role][i]}`);
  };
  return (
    <Drawer open={open} onClose={onClose} title="Add a widget" eyebrow="Widget library" media={<IconChip icon={Plus} tone="orange" />}>
      <div className="dlib" data-library>
        <p className="dlibg">Widgets</p>
        {types.map((t) => {
          const m = WIDGETS[t];
          const on = !m.many && hasTile(layout, t);
          const [Icon, tone] = WIDGET_ICONS[t];
          return (
            <button key={t} type="button" className="sb-row" disabled={on} onClick={() => add(t)} data-add-widget={t}>
              <IconChip icon={Icon} tone={tone} size="sm" />
              <div>
                <b>{m.name}</b>
                <small>{m.desc}</small>
              </div>
              {on ? <Badge kind="neutral">On Home</Badge> : <Badge kind="info">Add</Badge>}
            </button>
          );
        })}
        <p className="dlibg">Metrics</p>
        {KPI_NAMES[role].map((name, i) => {
          const on = hasTile(layout, "kpi", i);
          return (
            <button key={name} type="button" className="sb-row" disabled={on} onClick={() => add("kpi", i)} data-add-widget={`kpi${i}`}>
              <IconChip icon={WIDGET_ICONS.kpi[0]} tone="blue" size="sm" />
              <div>
                <b>{name}</b>
                <small>A key number at a glance</small>
              </div>
              {on ? <Badge kind="neutral">On Home</Badge> : <Badge kind="info">Add</Badge>}
            </button>
          );
        })}
      </div>
    </Drawer>
  );
}

export function PublishDrawer({ open, onClose, layout }: { open: boolean; onClose: () => void; layout: HomeLayout }) {
  const toast = useToast();
  const [roles, setRoles] = useState<Role[]>(["faculty"]);
  return (
    <Drawer
      open={open}
      onClose={onClose}
      title="Publish as default"
      eyebrow="Institution · new users start here"
      media={<IconChip icon={Send} tone="teal" />}
      footer={
        <>
          <Button
            icon={<Send strokeWidth={1.75} />}
            iconPosition="start"
            disabled={roles.length === 0}
            onClick={() => {
              roles.forEach((r) => publishDefault(r, layout));
              toast(`Published for ${roles.map((r) => ROLE_CONFIG[r].title).join(", ")}`);
              onClose();
            }}
            data-publish-ok
          >
            Publish
          </Button>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
        </>
      }
    >
      <p className="dh-lead">People who have not customized their Home start from this layout. Their own changes always win.</p>
      <div className="dh-presets">
        {ROLES.map((r) => {
          const on = roles.includes(r);
          return (
            <button key={r} type="button" className={`sb-row${on ? " is-on" : ""}`} role="checkbox" aria-checked={on} onClick={() => setRoles((cur) => (on ? cur.filter((x) => x !== r) : [...cur, r]))} data-publish-role={r}>
              <span className="sb-check" style={on ? { background: "var(--sb-success)", borderColor: "var(--sb-success)", color: "#fff" } : undefined}>
                {on && <Check strokeWidth={3} style={{ width: 14, height: 14 }} />}
              </span>
              <div>
                <b>{ROLE_CONFIG[r].title}s</b>
                <small>{r === "administrator" ? "Including you" : `Everyone signed in as ${ROLE_CONFIG[r].title.toLowerCase()}`}</small>
              </div>
            </button>
          );
        })}
      </div>
      <p className="dh-lead" style={{ fontSize: 12.5 }}>
        Saved on this device until institution-wide Home defaults are available. Role-specific tiles, like Early flags, are skipped for roles that can’t use them.
      </p>
    </Drawer>
  );
}

/** Colour, width and height of one tile (prototype `.dpop`). */
export function TilePopover({ tile, layout, commit, onClose }: { tile: HomeTile; layout: HomeLayout; commit: (next: HomeLayout) => void; onClose: () => void }) {
  const cur = tile.color ?? WIDGETS[tile.type].color ?? "default";
  return (
    <div className="dpop" role="dialog" aria-label={`Customize ${WIDGETS[tile.type].name}`} data-tile-pop={tile.id}>
      <div className="dpop-head">
        <b>{tile.title ?? WIDGETS[tile.type].name}</b>
        <button type="button" className="sb-icon-btn" aria-label="Close" onClick={onClose}>
          <X strokeWidth={1.75} />
        </button>
      </div>
      <p className="dl2">Colour</p>
      <div className="dsw">
        {SWATCHES.map(([key, label]) => {
          const tone = tileTone(key);
          return (
            <button
              key={key}
              type="button"
              className={`${cur === key ? "on" : ""}${key === "default" ? " def" : ""}`}
              style={tone.background ? { background: tone.background } : undefined}
              title={label}
              aria-label={label}
              onClick={() => commit(updateTile(layout, tile.id, { color: key === "default" ? undefined : key }))}
              data-swatch={key}
            />
          );
        })}
      </div>
      <p className="dl2">Width</p>
      <div className="dseg">
        {WIDTHS.filter(([w]) => w >= WIDGETS[tile.type].min).map(([w, l]) => (
          <button key={w} type="button" className={tile.w === w ? "on" : ""} onClick={() => commit(resizeTile(layout, tile.id, { w }))} data-width={w}>
            {l}
          </button>
        ))}
      </div>
      <p className="dl2">Height</p>
      <div className="dseg">
        {[1, 2, 3].map((h) => (
          <button key={h} type="button" className={tile.h === h ? "on" : ""} onClick={() => commit(resizeTile(layout, tile.id, { h }))} data-height={h}>
            {h === 1 ? "Short" : h === 2 ? "Tall" : "Taller"}
          </button>
        ))}
      </div>
    </div>
  );
}

/** Mounts whichever Home panel is open (the theme drawer is mounted by the shell: it works on every page). */
export function HomePanels({ layout, commit }: { layout: HomeLayout; commit: (next: HomeLayout, focusId?: string) => void }) {
  const { panel } = useHomeUi();
  const close = homeUi.closePanel;
  return (
    <>
      <LayoutsDrawer open={panel === "layouts"} onClose={close} layout={layout} commit={commit} />
      <LibraryDrawer open={panel === "library"} onClose={close} layout={layout} commit={commit} />
      <PublishDrawer open={panel === "publish"} onClose={close} layout={layout} />
    </>
  );
}
