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
}

/** The saved connection's name in a workspace's session key: the backend keys a further tab on the same connection
 *  as name + "\u001f" + n (`Open` in lib.rs), and that suffix is never shown. */
export const connName = (key: string) => key.split("\u001f")[0];
