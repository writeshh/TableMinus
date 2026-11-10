// Notices about an action (saved, copied, could not export) in one place: a toast at the bottom right, in the same
// stack as the jobs panel's cards. The run bar keeps only the state of the current run. Errors stay longer and say
// themselves to screen readers at once; any toast stays while the pointer is on it.

import { el, icon } from "../dom";
import { jobsPanel } from "../database/jobs";

export type ToastKind = "" | "busy" | "done" | "error";

const MAX = 3; // older toasts give way, so the stack never covers the window
const LIFE = { error: 10_000, other: 4_000 };

export function toast(text: string, kind: ToastKind = "") {
  if (!text || kind === "busy") return; // progress belongs to the run bar or a job card, not to a notice
  const panel = jobsPanel();
  for (const old of panel.querySelectorAll<HTMLElement>(".toast")) if (old.dataset.text === text) old.remove();
  const close = el("button", { type: "button", className: "icon-btn ghost", ariaLabel: "Dismiss" }, icon("close"));
  const t = el("div", { className: `toast ${kind}` }, el("span", { className: "toast-text", textContent: text }), close);
  t.dataset.text = text;
  t.setAttribute("role", kind === "error" ? "alert" : "status");
  panel.append(t);
  const all = panel.querySelectorAll(".toast");
  for (let i = 0; i < all.length - MAX; i++) all[i].remove();
  let timer = 0;
  const start = () => (timer = window.setTimeout(() => t.remove(), kind === "error" ? LIFE.error : LIFE.other));
  t.onpointerenter = () => clearTimeout(timer);
  t.onpointerleave = start;
  close.onclick = () => (clearTimeout(timer), t.remove());
  start();
}
