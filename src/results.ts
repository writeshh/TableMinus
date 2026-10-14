// The strip under a SQL tab's editor: a tab per result set, or all of them in one, and Messages, which lists every
// statement run with its outcome and time. main.ts runs the statements and draws the grids.

import { el } from "./dom";
import { highlightSql } from "./editor";
import "./results.css";

export type QueryResult = { columns: string[]; rows: (string | null)[][]; affected: number; truncated?: boolean };

/** A result set kept under a SQL tab. */
export type ResultTab = {
  id: number; title: string;
  /** The statement that returned it, and which of its results it is (a procedure can return several). */
  sql: string; index: number;
  result: QueryResult;
  /** Kept when the tab runs again. */
  pinned: boolean;
  /** The statement only reads, so running it again for more rows is harmless. */
  readOnly: boolean;
};

/** One line per statement run. */
export type Message = { sql: string; text: string; error: boolean; ms: number; at: number };

/** What `Results.shown` holds besides a result tab's id. */
export const MESSAGES = 0;
export const ALL = -1;

export type Results = { tabs: ResultTab[]; messages: Message[]; shown: number };

export type StripActions = {
  show: (id: number) => void;
  menu: (e: MouseEvent, r: ResultTab) => void;
  togglePin: (r: ResultTab) => void;
};

const fmt = (n: number) => n.toLocaleString();
export const rowsLabel = (r: QueryResult) =>
  r.columns.length ? `${fmt(r.rows.length)}${r.truncated ? "+" : ""} row${r.rows.length === 1 ? "" : "s"}` : `${fmt(r.affected)} affected`;

/** The strip's tabs. With `append`, every result shares one tab. */
export function renderStrip(nav: HTMLElement, results: Results, append: boolean, on: StripActions) {
  const button = (id: number, label: string, title: string, extra?: HTMLElement) => {
    const b = el("button", { className: "result-tab", title }, el("span", { textContent: label }), ...(extra ? [extra] : []));
    b.setAttribute("role", "tab");
    b.setAttribute("aria-selected", String(results.shown === id));
    b.onclick = () => on.show(id);
    return b;
  };
  const tabs = append
    ? results.tabs.length ? [button(ALL, `Results (${results.tabs.length})`, "Every result, one after another")] : []
    : results.tabs.map((r) => {
        const pin = el("span", { className: `pin${r.pinned ? " on" : ""}`, textContent: "⚲", title: r.pinned ? "Unpin" : "Pin: keep it when the tab runs again" });
        pin.onclick = (e) => (e.stopPropagation(), on.togglePin(r));
        const b = button(r.id, `${r.title} · ${rowsLabel(r.result)}`, r.sql, pin);
        b.oncontextmenu = (e) => on.menu(e, r);
        return b;
      });
  const errors = results.messages.filter((m) => m.error).length;
  const label = errors ? `Messages (${errors} failed)` : "Messages";
  const messages = button(MESSAGES, label, "Every statement run, its outcome and time");
  messages.classList.toggle("failed", errors > 0);
  nav.replaceChildren(...tabs, messages);
}

/** The Messages tab: what each statement did, oldest first. */
export function renderMessages(out: HTMLElement, messages: Message[], kind: string) {
  const time = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" });
  if (!messages.length) return out.replaceChildren(el("p", { className: "hint empty", textContent: "Run a query to see messages here." }));
  out.replaceChildren(
    el("ol", { className: "messages" },
      ...messages.map((m) => {
        const sql = highlightSql(m.sql.length > 2000 ? m.sql.slice(0, 2000) + "…" : m.sql, kind);
        sql.className = "message-sql";
        return el("li", { className: m.error ? "error" : "" },
          el("span", { className: "message-when", textContent: `${time.format(m.at)} · ${fmt(m.ms)} ms` }),
          sql,
          el("span", { className: "message-text", textContent: m.text }));
      })),
  );
}
