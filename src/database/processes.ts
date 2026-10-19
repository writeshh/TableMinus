// The process list window: the server's sessions and their queries, with cancel and kill (Rust: sql/processes.rs).

import { invoke } from "../ipc";
import { el } from "../dom";
import { modal, type Confirm, type Conn } from "./backup";

type QueryResult = { columns: string[]; rows: (string | null)[][] };

export function openProcesses(c: Conn, confirm: Confirm) {
  const table = el("table");
  const auto = el("input", { type: "checkbox", checked: true });
  const { d, footer, setStatus } = modal(`Processes on ${c.name}`, "tools-window processes", el("div", { className: "process-list" }, table));
  const refresh = el("button", { type: "button", textContent: "Refresh" });
  const close = el("button", { type: "button", className: "primary", textContent: "Close" });
  footer.append(el("label", { className: "check" }, auto, "Refresh every 3 s"), el("span", { className: "spacer" }), refresh, close);
  close.onclick = () => d.close();
  let busy = false;

  async function load() {
    if (busy) return;
    busy = true;
    try {
      render(await invoke<QueryResult>("processes", { name: c.name }));
      setStatus(`Updated ${new Date().toLocaleTimeString()}`);
    } catch (e) {
      setStatus(String(e), "error");
    } finally {
      busy = false;
    }
  }

  async function stop(id: string, connection: boolean) {
    const what = connection ? `Kill process ${id} and close its connection?` : `Cancel the query running in process ${id}?`;
    if (!(await confirm(what, connection ? "Kill" : "Cancel query"))) return;
    try {
      await invoke("kill_process", { name: c.name, id, connection });
      setStatus(connection ? `Killed ${id}` : `Cancelled the query in ${id}`, "done");
      load();
    } catch (e) {
      setStatus(String(e), "error");
    }
  }

  function render(r: QueryResult) {
    const button = (label: string, id: string, connection: boolean) => {
      const b = el("button", { type: "button", className: "icon", textContent: label });
      b.onclick = () => stop(id, connection);
      return b;
    };
    table.replaceChildren(
      el("thead", {}, el("tr", {}, el("th", { className: "process-actions" }), ...r.columns.map((col) => el("th", { textContent: col })))),
      el("tbody", {}, ...r.rows.map((row) => {
        const id = row[0] ?? "";
        // Actions first, so they stay in view however wide the query column is.
        return el("tr", {},
          el("td", { className: "process-actions" }, el("span", {}, button("Cancel query", id, false), button("Kill", id, true))),
          ...row.map((v) => el("td", { textContent: v ?? "", title: v ?? "" })));
      })),
    );
    if (!r.rows.length) table.append(el("caption", { textContent: "No other sessions." }));
  }

  refresh.onclick = load;
  const timer = setInterval(() => auto.checked && !document.querySelector("dialog#confirm[open]") && load(), 3000);
  d.addEventListener("close", () => {
    clearInterval(timer);
    invoke("close_processes", { name: c.name }).catch(() => {}); // the side connection; gone already is fine
  });
  d.showModal();
  setStatus("Loading…", "busy");
  load();
}
