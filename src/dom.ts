// Small DOM helpers shared by the UI modules.

import { revealItemInDir } from "@tauri-apps/plugin-opener";

export const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

/** An element with properties set and children appended. */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> = {},
  ...kids: (Node | string)[]
): HTMLElementTagNameMap[K] {
  const n = Object.assign(document.createElement(tag), props);
  n.append(...kids);
  return n;
}

/** Stroke paths on a 24×24 grid. Text glyphs (×, ›, ▸) sit off-centre in their line box, so icons are drawn instead. */
const ICONS = {
  close: "M6 6l12 12M18 6L6 18",
  plus: "M12 5v14M5 12h14",
  minus: "M5 12h14",
  chevron: "M9 6l6 6-6 6",
  copy: "M9 9h10v10H9zM5 15V5h10",
  only: "M8 5l11 7-11 7z",
  more: "M5.5 12h1M11.5 12h1M17.5 12h1",
  check: "M5 12.5l4.5 4.5L19 7.5",
} as const;

/** A centred stroke icon that takes its colour and size from the button around it (`svg.ico` in styles.css). */
export function icon(name: keyof typeof ICONS): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.classList.add("ico");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", ICONS[name]);
  svg.append(path);
  return svg;
}

/** A button that shows `paths` selected in the system's file manager, named for the platform's one. */
export function revealButton(paths: string[], className = "") {
  const ua = navigator.userAgent;
  const where = ua.includes("Mac") ? "Show in Finder" : ua.includes("Windows") ? "Show in Explorer" : "Show in folder";
  const b = el("button", { type: "button", className: `reveal ${className}`.trim(), textContent: where });
  // Best effort: a file moved or deleted since has nothing to show, and the job is done either way.
  b.onclick = () => void revealItemInDir(paths).catch(() => {});
  return b;
}

/** Writes a status line; its kind colours it. */
export function setStatus(id: string, text: string, kind: "" | "busy" | "error" | "done" = "") {
  const s = $(id);
  s.textContent = text;
  s.className = `status ${kind}`;
  s.title = text; // a line cut short shows the rest on hover
}

/** The saved connection's name in a workspace's session key: the backend keys a further tab on the same connection
 *  as name + "\u001f" + n (`Open` in lib.rs), and that suffix is never shown. */
export const connName = (key: string) => key.split("\u001f")[0];
