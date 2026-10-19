// Query history in the sidebar. It lives per connection in history.json (Settings › Locations), newest first, for as
// long as Settings › General keeps it, and at most 5,000 per connection (history-list.ts).

import { $, el } from "./dom";
import { byDay, keptSince, prepend, type HistoryEntry } from "./history-list";
import { S } from "./state";
import { docEntry, setDocEntry } from "./locations";
import { settings } from "./settings";
import { confirmDialog, contextMenu } from "./dialogs";
import { addFavorite } from "./favorites";
import { copyText } from "./gridview";

/** What an entry opens into (main.ts). */
type Host = {
  /** Into the SQL tab on screen, or a new one when a table tab shows. */
  load(sql: string): void;
  /** Into a new SQL tab. */
  openNew(sql: string): void;
};
let host: Host;

const loadHistory = () => (S.active ? keptSince(docEntry<HistoryEntry[]>("history", S.active.name), settings.historyYears, Date.now()) : []);

export function addHistory(sql: string) {
  if (!S.active) return;
  setDocEntry("history", S.active.name, prepend({ sql: sql.trim(), at: Date.now(), db: S.info?.database ?? null }, loadHistory()));
  renderHistory();
}

async function clearHistory() {
  if (!S.active || !loadHistory().length) return;
  if (!(await confirmDialog(`Clear the query history of "${S.active.name}"?`, "Clear"))) return;
  setDocEntry("history", S.active.name, undefined);
  renderHistory();
}

const dayFormat = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "long", year: "numeric" });
const timeFormat = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" });

export function renderHistory() {
  const groups = byDay(loadHistory(), $<HTMLInputElement>("history-search").value, (at) => dayFormat.format(at));
  $("history").replaceChildren(
    ...[...groups].map(([day, entries], i) => {
      const d = el("details", { open: i === 0 }, el("summary", { textContent: day }));
      for (const h of entries) {
        const item = el("button", { className: "history-item", title: "Open in the SQL editor" },
          el("span", { className: "when", textContent: timeFormat.format(h.at) + (h.db ? ` · ${h.db}` : "") }),
          el("code", { textContent: h.sql }),
        );
        item.onclick = () => host.load(h.sql);
        item.oncontextmenu = (e) => contextMenu(e, [
          ["Open in a new tab", () => host.openNew(h.sql)],
          ["Save as favorite…", () => addFavorite(h.sql)],
          ["Copy SQL", () => copyText(h.sql, "SQL")],
        ]);
        d.append(item);
      }
      return d;
    }),
  );
  $("no-history").hidden = groups.size > 0;
}

export function initHistory(h: Host) {
  host = h;
  $("history-search").addEventListener("input", renderHistory);
  $("clear-history").onclick = clearHistory;
}
