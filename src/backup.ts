// Backup and restore windows, and the panel that follows their jobs while they run (Rust side: backup.rs).

import { invoke } from "./ipc";
import { listen } from "@tauri-apps/api/event";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { el, revealButton } from "./dom";
import { jobsPanel, notify } from "./jobs";
import { guarded } from "./safety";

export type Conn = { name: string; kind: string };
export type Confirm = (text: string, okLabel: string) => Promise<boolean>;
type Tools = { dir: string; version: string; mariadb: boolean };
type Setup = {
  server: string; databases: string[]; database: string | null;
  tools: Tools[]; matched: number | null; problem: string | null;
};

/** A modal window built for one use and removed when it closes. */
export function modal(title: string, className: string, ...body: Node[]) {
  const status = el("p", { className: "status" });
  const footer = el("footer");
  const d = el("dialog", { className }, el("h3", { textContent: title }), ...body, status, footer);
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  const setStatus = (text: string, kind: "" | "busy" | "error" | "done" = "") => {
    status.textContent = text;
    status.className = `status ${kind}`;
  };
  return { d, footer, setStatus };
}

const row = (label: string, ...kids: (Node | string)[]) => el("label", {}, label, ...kids);
const pick = (input: HTMLInputElement, onPick: () => void) => {
  const b = el("button", { type: "button", textContent: "Choose…" });
  b.onclick = onPick;
  return el("span", { className: "pick" }, input, b);
};
const option = (value: string, label = value) => el("option", { value, textContent: label });
const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** The client-tools picker: every version found, the one matching the server chosen, and Choose… for others. */
function toolsPicker(setup: Setup) {
  const select = el("select");
  if (setup.matched === null) select.append(option("", "No matching tools found"));
  setup.tools.forEach((t, i) =>
    select.append(option(t.dir, `${t.mariadb ? "MariaDB" : ""} ${t.version}${i === setup.matched ? " (matches server)" : ""} · ${t.dir}`.trim())),
  );
  if (setup.matched !== null) select.value = setup.tools[setup.matched].dir;
  const hint = el("p", { className: "hint", textContent: setup.problem ?? "" });
  hint.hidden = !setup.problem;
  const choose = el("button", { type: "button", textContent: "Choose…" });
  choose.onclick = async () => {
    const path = await openFileDialog({ title: "Choose a dump or client tool (its folder is used)" });
    if (typeof path !== "string") return;
    select.append(option(path, `Chosen · ${path}`));
    select.value = path;
    hint.hidden = true;
  };
  return { node: row("Tools", el("span", { className: "pick" }, select, choose)), hint, value: () => select.value || null };
}

async function loadSetup(c: Conn, setStatus: (t: string, k?: "" | "busy" | "error" | "done") => void) {
  setStatus(`Connecting to ${c.name}…`, "busy");
  try {
    const s = await invoke<Setup>("backup_setup", { name: c.name });
    setStatus("");
    return s;
  } catch (e) {
    setStatus(String(e), "error");
    return null;
  }
}

export async function openBackup(c: Conn, database?: string | null) {
  const lite = c.kind === "sqlite";
  const db = el("select");
  const contents = el("select", {}, option("all", "Schema and data"), option("schema", "Schema only"), option("data", "Data only"));
  const format = el("select", {}, option("sql", "Plain SQL"), option("custom", "Custom archive (pg_restore)"));
  const flags = el("input", { placeholder: c.kind === "postgres" ? "e.g. --no-owner --exclude-table=logs" : "e.g. --routines --events" });
  const gzip = el("input", { type: "checkbox" });
  const path = el("input", { placeholder: "Choose where to save" });
  const settings = el("fieldset", {}, row("Database", db), row("Contents", contents), row("Format", format), row("Custom flags", flags),
    el("label", { className: "check" }, gzip, "Compress with gzip"));
  const { d, footer, setStatus } = modal(`Back up ${c.name}`, "tools-window", settings, el("fieldset", {}, row("File", pick(path, () => choosePath()))));
  format.parentElement!.hidden = c.kind !== "postgres";
  const cancel = el("button", { type: "button", textContent: "Cancel" });
  const start = el("button", { type: "button", className: "primary", textContent: "Start backup", disabled: true });
  cancel.onclick = () => d.close();
  footer.append(cancel, el("span", { className: "spacer" }), start);
  d.showModal();

  const ext = () => (lite ? ".sqlite" : format.value === "custom" ? ".dump" : ".sql") + (gzip.checked && !lite ? ".gz" : "");
  const defaultName = () => `${lite ? fileName(c.name) : db.value || c.name}-${new Date().toISOString().slice(0, 10)}${ext()}`;
  async function choosePath() {
    const p = await saveFileDialog({ defaultPath: path.value || defaultName() });
    if (typeof p === "string") path.value = p;
  }
  // gzip on a custom archive (already compressed) only costs time
  format.onchange = () => ((gzip.parentElement!.hidden = format.value === "custom"), format.value === "custom" && (gzip.checked = false));

  const setup = await loadSetup(c, setStatus);
  if (!setup) return;
  let tools: ReturnType<typeof toolsPicker> | null = null;
  if (lite) {
    settings.hidden = true; // one file, nothing to choose but where it goes
  } else {
    db.append(...setup.databases.map((n) => option(n)));
    db.value = database ?? setup.database ?? setup.databases[0] ?? "";
    tools = toolsPicker(setup);
    db.closest("label")!.after(tools.node, tools.hint);
  }
  start.disabled = false;
  start.onclick = async () => {
    if (!path.value) await choosePath();
    if (!path.value) return;
    const request = {
      database: db.value, tools: tools?.value() ?? null, contents: contents.value, format: format.value,
      gzip: gzip.checked, flags: flags.value, path: path.value,
    };
    start.disabled = true;
    setStatus("Starting…", "busy");
    try {
      const id = await invoke<number>("start_backup", { name: c.name, request });
      d.close();
      addJob(id, `Backing up ${lite ? c.name : db.value}`, fileName(path.value), path.value);
    } catch (e) {
      setStatus(String(e), "error");
      start.disabled = false;
    }
  };
}

export async function openRestore(c: Conn, database: string | null | undefined, confirm: Confirm) {
  const lite = c.kind === "sqlite";
  const path = el("input", { placeholder: "A backup file (.sql, .sql.gz, .dump)" });
  const into = el("select");
  const newName = el("input", { placeholder: "new database name" });
  const flags = el("input", { placeholder: c.kind === "postgres" ? "e.g. --no-owner --clean" : "e.g. --force" });
  const target = el("fieldset", {}, row("Into", el("span", { className: "pick" }, into, newName)), row("Custom flags", flags));
  const { d, footer, setStatus } = modal(`Restore into ${c.name}`, "tools-window", el("fieldset", {}, row("File", pick(path, () => chooseFile()))), target);
  newName.hidden = true;
  into.onchange = () => ((newName.hidden = into.value !== ""), !newName.hidden && newName.focus());
  const cancel = el("button", { type: "button", textContent: "Cancel" });
  const start = el("button", { type: "button", className: "danger", textContent: "Restore…", disabled: true });
  cancel.onclick = () => d.close();
  footer.append(cancel, el("span", { className: "spacer" }), start);
  d.showModal();

  async function chooseFile() {
    const p = await openFileDialog({ title: "Choose a backup to restore" });
    if (typeof p === "string") path.value = p;
  }

  const setup = await loadSetup(c, setStatus);
  if (!setup) return;
  let tools: ReturnType<typeof toolsPicker> | null = null;
  if (lite) {
    target.hidden = true;
    target.after(el("p", { className: "hint", textContent: "The backup replaces everything in this database file." }));
  } else {
    into.append(...setup.databases.map((n) => option(n)), option("", "New database…"));
    into.value = database ?? setup.database ?? setup.databases[0] ?? "";
    newName.hidden = into.value !== "";
    tools = toolsPicker(setup);
    target.append(tools.node, tools.hint);
  }
  start.disabled = false;
  start.onclick = async () => {
    if (!path.value) await chooseFile();
    if (!path.value) return;
    const create = !lite && into.value === "";
    const name = create ? newName.value.trim() : into.value;
    if (!lite && !name) return setStatus("Name the new database.", "error");
    const where = lite ? c.name : create ? `a new database "${name}"` : `"${name}"`;
    if (!(await confirm(`Restore ${fileName(path.value)} into ${where}? Existing tables and data can be overwritten.`, "Restore"))) return;
    start.disabled = true;
    setStatus("Starting…", "busy");
    try {
      const request = { database: name, create, tools: tools?.value() ?? null, flags: flags.value, path: path.value };
      const id = await guarded(c.name, (a) => invoke<number>("start_restore", { name: c.name, request, confirmed: true, unlock: a.unlock }));
      d.close();
      addJob(id, `Restoring into ${lite ? c.name : name}`, fileName(path.value));
    } catch (e) {
      setStatus(String(e), "error");
      start.disabled = false;
    }
  };
}

// ---------- Jobs panel: bottom right, one card per job ----------

type Progress = { id: number; bytes: number | null; total: number | null; line: string | null };
type Done = { id: number; error: string | null };
/** `path` is the backup file to show when done; a restore has nothing new to show. */
type Card = { title: string; bytes: HTMLElement; line: HTMLElement; bar: HTMLProgressElement; button: HTMLButtonElement; root: HTMLElement; path: string | null };

const cards = new Map<number, Card>();
// Events that beat start_backup's answer to the UI, applied once the card exists.
const early = new Map<number, (Progress | Done)[]>();

function size(n: number) {
  const units = ["bytes", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) (n /= 1024), i++;
  return `${i ? n.toFixed(1) : n} ${units[i]}`;
}

function addJob(id: number, title: string, file: string, path: string | null = null) {
  const panel = jobsPanel();
  const bytes = el("span", { className: "job-bytes" });
  const line = el("div", { className: "job-line" });
  const bar = el("progress");
  const button = el("button", { type: "button", className: "icon", textContent: "Cancel" });
  button.onclick = () => invoke("cancel_backup", { id });
  const root = el("div", { className: "job" }, el("div", { className: "job-head" }, el("strong", { textContent: title }), el("span", { className: "spacer" }), button),
    el("div", { className: "job-file", textContent: file }), bar, el("div", { className: "job-meta" }, bytes), line);
  panel.append(root);
  cards.set(id, { title, bytes, line, bar, button, root, path });
  for (const e of early.get(id) ?? []) apply(e);
  early.delete(id);
}

function apply(e: Progress | Done) {
  const card = cards.get(e.id);
  if (!card) return void early.set(e.id, [...(early.get(e.id) ?? []), e]);
  if ("bytes" in e) {
    if (e.line) (card.line.textContent = e.line), (card.line.title = e.line);
    if (e.bytes !== null) {
      card.bytes.textContent = e.total ? `${size(e.bytes)} of ${size(e.total)}` : size(e.bytes);
      if (e.total) (card.bar.max = e.total), (card.bar.value = Math.min(e.bytes, e.total));
    }
    return;
  }
  card.bar.hidden = true;
  card.root.classList.add(e.error ? "failed" : "finished");
  card.line.textContent = e.error === null ? "Done" : e.error === "cancelled" ? "Cancelled" : e.error;
  card.line.title = card.line.textContent;
  card.button.textContent = "Dismiss";
  if (e.error === null && card.path) card.button.before(revealButton([card.path], "icon"));
  notify(card.title, card.line.textContent, e.error === null, e.error === null && card.path ? [card.path] : []);
  card.button.onclick = () => {
    card.root.remove();
    cards.delete(e.id);
  };
}

// No event bridge outside the app (the vite preview without Tauri).
listen<Progress>("backup-progress", (e) => apply(e.payload)).catch(() => {});
listen<Done>("backup-done", (e) => apply(e.payload)).catch(() => {});
