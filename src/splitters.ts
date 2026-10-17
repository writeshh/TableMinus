// Draggable dividers between the workspace's panes (the sidebar's right edge, the SQL editor's bottom edge, the
// console's top edge) and the commands that hide panes. Each size is a CSS variable on #workspace, kept in settings
// (prefs.ts) so it survives a relaunch; double-click or Home puts the default back. A divider takes focus, and the
// arrow keys move it 16px (64 with Shift).

import { $, el } from "./dom";
import { readPref, writePref } from "./prefs";

export type Divider = {
  /** Where the handle goes; its CSS places it on the pane's edge. */
  host: HTMLElement;
  className: string;
  /** The custom property it sets on #workspace, in px. */
  variable: string;
  /** Where the size is kept (prefs.ts). */
  pref: string;
  axis: "x" | "y";
  /** 1 when moving right or down grows the pane, -1 when it shrinks it (the console grows upward). */
  sign: 1 | -1;
  min: number;
  /** The most the pane may take with the space there is now. */
  max: () => number;
  /** The pane's size now, in px. */
  size: () => number;
  label: string;
};

const STEP = 16;

export function divider(d: Divider) {
  const root = $("workspace");
  const handle = el("div", { className: `divider ${d.axis} ${d.className}`, tabIndex: 0, title: `${d.label}: drag to resize, double-click to reset` });
  handle.setAttribute("role", "separator");
  handle.setAttribute("aria-orientation", d.axis === "x" ? "vertical" : "horizontal");
  handle.setAttribute("aria-label", d.label);
  const clamp = (px: number) => Math.round(Math.max(d.min, Math.min(px, Math.max(d.min, d.max()))));
  /** Sizes the pane (`null`: its CSS default); `keep` also saves it, which a drag does once, at the end. */
  const set = (px: number | null, keep = true) => {
    const value = px === null ? undefined : clamp(px);
    if (value === undefined) root.style.removeProperty(d.variable);
    else root.style.setProperty(d.variable, `${value}px`);
    if (keep) writePref(d.pref, value);
  };
  const saved = readPref<number>(d.pref);
  // Not clamped here: the layout isn't measured yet. The CSS caps it (min()), and the next move clamps it.
  if (typeof saved === "number" && Number.isFinite(saved)) root.style.setProperty(d.variable, `${Math.max(d.min, saved)}px`);

  handle.onpointerdown = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    handle.setPointerCapture(e.pointerId);
    handle.classList.add("active");
    const at = (ev: PointerEvent) => (d.axis === "x" ? ev.clientX : ev.clientY);
    const start = at(e);
    const from = d.size();
    let last = from;
    const done = new AbortController();
    handle.addEventListener("pointermove", (ev) => set((last = from + d.sign * (at(ev) - start)), false), { signal: done.signal });
    const end = () => {
      done.abort();
      handle.classList.remove("active");
      set(last);
    };
    handle.addEventListener("pointerup", end, { signal: done.signal });
    handle.addEventListener("pointercancel", end, { signal: done.signal });
  };
  handle.ondblclick = () => set(null);
  handle.onkeydown = (e) => {
    const back = d.axis === "x" ? "ArrowLeft" : "ArrowUp";
    const forth = d.axis === "x" ? "ArrowRight" : "ArrowDown";
    const step = e.shiftKey ? STEP * 4 : STEP;
    const by = e.key === back ? -step : e.key === forth ? step : 0;
    if (!by && e.key !== "Home") return;
    e.preventDefault();
    e.stopPropagation(); // not the grid's arrow keys
    set(by ? d.size() + d.sign * by : null);
  };
  d.host.append(handle);
  return handle;
}

// ---------- Hiding panes ----------

export type Hideable = "sidebar" | "results";
const HIDDEN_PREF = "layout.hidden";

/** Applies the kept choices at startup. */
export function initHiding() {
  const kept = readPref<Partial<Record<Hideable, boolean>>>(HIDDEN_PREF) ?? {};
  for (const which of ["sidebar", "results"] as const) $("workspace").classList.toggle(`no-${which}`, kept[which] === true);
}

/** Shows or hides a pane, and keeps the choice. */
export function togglePane(which: Hideable) {
  const hidden = $("workspace").classList.toggle(`no-${which}`);
  writePref(HIDDEN_PREF, { ...readPref<Partial<Record<Hideable, boolean>>>(HIDDEN_PREF), [which]: hidden });
}
