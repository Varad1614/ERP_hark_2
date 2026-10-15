/**
 * Home personalisation stores (re-skin v2): the layout per user + role, the customize / panel UI
 * state shared by the shell menu and Home, focus mode, and the personal theme. Everything lives
 * in localStorage (`sutra.home.v3.<login>.<role>`, `sutra.theme.v1`); no API is involved.
 */
import { useSyncExternalStore } from "react";
import type { Role } from "../../lib/roles";
import { builtInLayout, isHex, sanitizeLayout, type HomeLayout } from "./model";

function makeStore<T>(initial: T) {
  let state = initial;
  const listeners = new Set<() => void>();
  return {
    get: () => state,
    set(next: T) {
      state = next;
      listeners.forEach((l) => l());
    },
    /** Replaces the state without notifying (safe during render; the caller reads it next). */
    replace(next: T) {
      state = next;
    },
    subscribe(cb: () => void) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
  };
}

function readJson(key: string): unknown {
  try {
    return JSON.parse(localStorage.getItem(key) ?? "null");
  } catch {
    return null;
  }
}
function writeJson(key: string, v: unknown) {
  try {
    if (v === undefined) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(v));
  } catch {
    /* private mode / quota */
  }
}

/* ------------------------------------------------------------------ layout */

const layoutKey = (partition: string) => `sutra.home.v3.${partition}`;
const defaultKey = (role: Role) => `sutra.home.default.${role}`;

interface LayoutState {
  partition: string | null;
  role: Role;
  /** The user's own layout (undefined = the published default or the built-in one). */
  own?: HomeLayout;
  /** A layout an administrator published as the default for the role (this browser). */
  published?: HomeLayout;
}

const layouts = makeStore<LayoutState>({ partition: null, role: "administrator" });

/** Binds the layout store to the signed-in user + role. */
export function bindLayout(partition: string, role: Role) {
  const s = layouts.get();
  if (s.partition === partition && s.role === role) return;
  layouts.replace({ partition, role, own: sanitizeLayout(readJson(layoutKey(partition))), published: sanitizeLayout(readJson(defaultKey(role))) });
}

export function effectiveLayout(s: LayoutState = layouts.get()): HomeLayout {
  return s.own ?? s.published ?? builtInLayout(s.role);
}

export function setLayout(next: HomeLayout | undefined) {
  const s = layouts.get();
  if (s.partition) writeJson(layoutKey(s.partition), next);
  layouts.set({ ...s, own: next });
}

/** Publishes the current layout as the default for a role (stored in this browser; no API yet). */
export function publishDefault(role: Role, layout: HomeLayout) {
  const clean = { items: layout.items.filter((x) => x.type !== "note"), ...(layout.preset ? { preset: layout.preset } : {}) };
  writeJson(defaultKey(role), clean);
  const s = layouts.get();
  if (s.role === role) layouts.set({ ...s, published: clean });
}

export function useLayoutState(): LayoutState {
  return useSyncExternalStore(layouts.subscribe, layouts.get, layouts.get);
}

/* ------------------------------------------------------------------ customize UI */

export type HomePanel = "library" | "layouts" | "theme" | "publish" | null;
interface UiState {
  editing: boolean;
  panel: HomePanel;
  focus: boolean;
}
const ui = makeStore<UiState>({ editing: false, panel: null, focus: false });

export const homeUi = {
  startCustomize: () => ui.set({ ...ui.get(), editing: true, focus: false }),
  stopCustomize: () => ui.set({ ...ui.get(), editing: false, panel: ui.get().panel === "theme" ? "theme" : null }),
  openPanel: (panel: HomePanel) => ui.set({ ...ui.get(), panel }),
  closePanel: () => ui.set({ ...ui.get(), panel: null }),
  setFocus: (focus: boolean) => {
    ui.set({ ...ui.get(), focus, editing: focus ? false : ui.get().editing });
    if (typeof document !== "undefined") document.documentElement.classList.toggle("dfocus", focus);
  },
  get: ui.get,
};

export function useHomeUi(): UiState {
  return useSyncExternalStore(ui.subscribe, ui.get, ui.get);
}

/* ------------------------------------------------------------------ theme */

export type ThemeMode = "light" | "dark" | "auto" | "oled";
export interface ThemePrefs {
  mode: ThemeMode;
  /** Accent override (hex); undefined = the role accent. */
  accent?: string;
  /** Page background (hex); undefined = cream. */
  bg?: string;
  radius: number;
  gap: number;
  dense: boolean;
  shadow: "flat" | "soft" | "raised";
  font: "sutra" | "system" | "serif" | "easy";
  hc: boolean;
  scale: number;
}

export const DEFAULT_THEME: ThemePrefs = { mode: "light", radius: 26, gap: 16, dense: false, shadow: "soft", font: "sutra", hc: false, scale: 1 };
export const ACCENTS = ["#E8743B", "#D6336C", "#7254E6", "#3B7BE8", "#1F8A8A", "#2E8B57", "#A77A0B", "#1C1B22"];
export const BACKGROUNDS: ReadonlyArray<readonly [string, string]> = [["#FAF8F4", "Cream"], ["#FFFFFF", "White"], ["#F2F3F6", "Cloud"], ["#EEF6F1", "Sage"], ["#F3F0FA", "Lavender"], ["#FBF1EA", "Sand"]];
const THEME_KEY = "sutra.theme.v1";

export function sanitizeTheme(raw: unknown): ThemePrefs {
  const t = (raw && typeof raw === "object" ? raw : {}) as Partial<ThemePrefs>;
  // First run: carry over the display prefs chosen before the theme drawer existed.
  const legacy = (readJson("sutra.display") ?? {}) as { mode?: string; contrast?: string; font?: string };
  const mode = t.mode ?? legacy.mode;
  return {
    mode: mode === "dark" || mode === "auto" || mode === "oled" ? mode : "light",
    ...(isHex(t.accent) ? { accent: t.accent } : {}),
    ...(isHex(t.bg) ? { bg: t.bg } : {}),
    radius: typeof t.radius === "number" && t.radius >= 4 && t.radius <= 36 ? t.radius : 26,
    gap: t.gap === 10 || t.gap === 24 ? t.gap : 16,
    dense: t.dense === true,
    shadow: t.shadow === "flat" || t.shadow === "raised" ? t.shadow : "soft",
    font: t.font === "system" || t.font === "serif" || t.font === "easy" ? t.font : t.font === undefined && legacy.font === "easy" ? "easy" : "sutra",
    hc: t.hc === undefined ? legacy.contrast === "high" : t.hc === true,
    scale: t.scale === 0.9 || t.scale === 1.1 ? t.scale : 1,
  };
}

const theme = makeStore<ThemePrefs>(typeof localStorage === "undefined" ? DEFAULT_THEME : sanitizeTheme(readJson(THEME_KEY)));

export function setTheme(patch: Partial<ThemePrefs> | null) {
  const next = patch === null ? DEFAULT_THEME : sanitizeTheme({ ...theme.get(), ...patch });
  writeJson(THEME_KEY, next);
  // Keep the R0 display prefs key in step (dark / contrast / font classes, read at boot).
  writeJson("sutra.display", { mode: next.mode === "oled" ? "dark" : next.mode, contrast: next.hc ? "high" : "standard", font: next.font === "easy" ? "easy" : "sutra" });
  theme.set(next);
}

export function useTheme(): ThemePrefs {
  return useSyncExternalStore(theme.subscribe, theme.get, theme.get);
}
export const themeSnapshot = theme.get;
