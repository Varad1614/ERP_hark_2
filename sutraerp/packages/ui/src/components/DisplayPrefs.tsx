import { useCallback, useEffect, useState } from "react";

/** Display preferences of the prototype's Personalization menu: theme, contrast, font. */
export interface DisplayPrefs {
  mode: "light" | "dark" | "auto";
  contrast: "standard" | "high";
  font: "sutra" | "easy";
}

export const DEFAULT_DISPLAY_PREFS: DisplayPrefs = { mode: "light", contrast: "standard", font: "sutra" };
const KEY = "sutra.display";

export function readDisplayPrefs(): DisplayPrefs {
  try {
    const raw = typeof localStorage === "undefined" ? null : localStorage.getItem(KEY);
    const p = raw ? (JSON.parse(raw) as Partial<DisplayPrefs>) : {};
    return {
      mode: p.mode === "dark" || p.mode === "auto" ? p.mode : "light",
      contrast: p.contrast === "high" ? "high" : "standard",
      font: p.font === "easy" ? "easy" : "sutra",
    };
  } catch {
    return DEFAULT_DISPLAY_PREFS;
  }
}

function prefersDark(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** Applies the prefs as classes on <html>: `dk` (dark), `hc` (high contrast), `easy` (Atkinson Hyperlegible). */
export function applyDisplayPrefs(p: DisplayPrefs, el: HTMLElement = document.documentElement) {
  el.classList.toggle("dk", p.mode === "dark" || (p.mode === "auto" && prefersDark()));
  el.classList.toggle("hc", p.contrast === "high");
  el.classList.toggle("easy", p.font === "easy");
}

/** Persisted display prefs (localStorage `sutra.display`), applied to <html> on change. */
export function useDisplayPrefs(): [DisplayPrefs, (patch: Partial<DisplayPrefs>) => void] {
  const [prefs, setPrefs] = useState<DisplayPrefs>(readDisplayPrefs);
  useEffect(() => {
    applyDisplayPrefs(prefs);
    if (prefs.mode !== "auto" || typeof window.matchMedia !== "function") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const on = () => applyDisplayPrefs(prefs);
    mq.addEventListener?.("change", on);
    return () => mq.removeEventListener?.("change", on);
  }, [prefs]);
  const update = useCallback((patch: Partial<DisplayPrefs>) => {
    setPrefs((cur) => {
      const next = { ...cur, ...patch };
      try {
        localStorage.setItem(KEY, JSON.stringify(next));
      } catch {
        /* private mode */
      }
      return next;
    });
  }, []);
  return [prefs, update];
}

/** Segmented control (prototype `.dseg`). */
export function Segmented<T extends string>({ label, value, options, onChange }: { label: string; value: T; options: ReadonlyArray<readonly [T, string]>; onChange: (v: T) => void }) {
  return (
    <div className="sb-pop-setting">
      <span>{label}</span>
      <div className="sb-seg" role="radiogroup" aria-label={label}>
        {options.map(([v, l]) => (
          <button key={v} type="button" role="radio" aria-checked={v === value} className={v === value ? "on" : ""} data-display={`${label.toLowerCase()}:${v}`} onClick={() => onChange(v)}>
            {l}
          </button>
        ))}
      </div>
    </div>
  );
}
