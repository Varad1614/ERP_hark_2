/**
 * Applies the personal theme (prototype theme drawer) to <html>: dark / OLED / high-contrast /
 * easy-read classes, accent and background overrides, corner radius, spacing, tile depth, font
 * and text size. Role accents stay the default (RoleTheme); an accent here overrides them.
 */
import { applyDisplayPrefs } from "@sutra/ui";
import { useEffect } from "react";
import { useTheme, type ThemePrefs } from "./store";

/** Mixes two hex colours (`t` = share of `b`). */
export function mix(a: string, b: string, t: number): string {
  const p = (h: string) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  const x = p(a);
  const y = p(b);
  return `#${x.map((v, i) => Math.round(v + ((y[i] ?? v) - v) * t).toString(16).padStart(2, "0")).join("")}`;
}

const FONTS: Record<ThemePrefs["font"], string | null> = {
  sutra: null,
  system: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif',
  serif: 'Georgia, "Times New Roman", serif',
  easy: null,
};

const SHADOWS: Record<ThemePrefs["shadow"], string | null> = {
  flat: "none",
  soft: null,
  raised: "0 1px 2px rgba(0,0,0,.05), 0 12px 32px -12px rgba(0,0,0,.18)",
};

function prefersDark(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function applyTheme(t: ThemePrefs, el: HTMLElement = document.documentElement) {
  const dark = t.mode === "dark" || t.mode === "oled" || (t.mode === "auto" && prefersDark());
  applyDisplayPrefs({ mode: dark ? "dark" : "light", contrast: t.hc ? "high" : "standard", font: t.font === "easy" ? "easy" : "sutra" }, el);
  el.classList.toggle("oled", dark && t.mode === "oled");
  const set = (k: string, v: string | null) => (v === null ? el.style.removeProperty(k) : el.style.setProperty(k, v));
  if (t.accent) {
    const ac = dark && t.accent === "#1C1B22" ? "#EDEBF3" : t.accent;
    set("--ac", ac);
    set("--acs", dark ? mix(ac, "#1E1D24", 0.74) : mix(ac, "#FFFFFF", 0.82));
    set("--act", dark ? mix(ac, "#FFFFFF", 0.35) : mix(ac, "#000000", 0.32));
  } else {
    set("--ac", null);
    set("--acs", null);
    set("--act", null);
  }
  set("--sb-bg", t.bg && !(dark && t.mode === "oled") ? (dark ? mix(t.bg, "#111015", 0.9) : t.bg) : null);
  set("--radius-tile", t.radius === 26 ? null : `${t.radius}px`);
  set("--rad", `${t.radius}px`);
  set("--gap", t.gap === 16 ? null : `${t.gap}px`);
  set("--shadow-tile", SHADOWS[t.shadow]);
  set("--sb-font-override", FONTS[t.font]);
  el.classList.toggle("font-override", FONTS[t.font] !== null);
  el.style.fontSize = t.scale === 1 ? "" : `${t.scale * 100}%`;
  el.classList.toggle("dense-home", t.dense);
}

/** Keeps <html> in step with the theme store (and the device setting in Auto). */
export function useApplyTheme() {
  const t = useTheme();
  useEffect(() => {
    applyTheme(t);
    if (t.mode !== "auto" || typeof window.matchMedia !== "function") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const on = () => applyTheme(t);
    mq.addEventListener?.("change", on);
    return () => mq.removeEventListener?.("change", on);
  }, [t]);
}
