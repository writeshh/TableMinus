// Plugins (plugins.rs): Settings › Plugins lists them and installs, updates and removes them; their actions show in
// the grid's row menu and the sidebar's table menu. Installing shows what the plugin is and asks for each permission
// before it is granted. Actions run in the backend's QuickJS sandbox; their result is copied, put in the editor or
// shown, as the action asks.

import { invoke } from "./ipc";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { $, el } from "./dom";
import { modal } from "./backup";

type Permission = "query";
type Target = "rows" | "table";
type Action = { id: string; title: string; on: Target; run: string };
type Manifest = { id: string; name: string; version: string; description: string; author: string; permissions: Permission[]; main: string; actions: Action[] };
type Origin = { git: string } | { folder: string };
type Installed = { id: string; manifest: Manifest | null; problem: string | null; origin: Origin; commit: string; granted: Permission[]; enabled: boolean };
type Staged = { token: string; manifest: Manifest; commit: string; granted: Permission[] | null };
type Output = { clipboard: string | null; insert: string | null; message: string | null };
type MenuItem = [string, () => void] | "-";

const PERMISSIONS: Record<Permission, string> = {
  query: "Run read-only SQL on the open connection (safe mode still applies)",
};
const WHERE: Record<Target, string> = { rows: "the grid's row menu", table: "the sidebar's table menu" };

export type PluginCtx = {
  /** The on-screen connection: `name` is the session key commands take. */
  conn: () => { name: string; kind: string } | null;
  insert: (sql: string) => void;
  copy: (text: string, what: string) => void;
  say: (text: string, kind?: "done" | "error") => void;
};
let ctx: PluginCtx;
let installed: Installed[] = [];

// ---------- Settings › Plugins ----------

const settings = $("settings");
const tab = el("button");
tab.setAttribute("role", "tab");
tab.dataset.tab = "plugins";
tab.append(icon("M9 3v4M15 3v4M6 7h12v5a6 6 0 0 1-12 0zM12 18v3"), "Plugins");
settings.querySelector(".prefs-tabs")!.append(tab);
const pane = el("section", { hidden: true });
pane.dataset.pane = "plugins";
const list = el("ul", { className: "mcp-list plugin-list" });
const gitUrl = el("input", { type: "url", placeholder: "https://github.com/someone/tableminus-plugin", spellcheck: false });
const fromGit = el("button", { textContent: "Install from Git" });
const fromFolder = el("button", { className: "ghost", textContent: "Install from a folder…" });
const note = el("p", { className: "status" });
pane.append(
  el("p", { className: "hint note", textContent: "Plugins add actions to the grid's row menu and the sidebar's table menu. They run in a sandbox without files, network or the rest of the app, for at most ten seconds; a plugin reads the database only if you grant it that." }),
  el("div", { className: "plugin-install" }, gitUrl, fromGit, fromFolder),
  note,
  list,
);
settings.querySelector(":scope > footer")!.before(pane);

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
section[data-pane="plugins"] { max-height: min(560px, 68vh); overflow-y: auto; }
.plugin-install { display: flex; gap: 6px; }
.plugin-install input { flex: 1; }
.plugin-list li { align-items: flex-start; }
.plugin-list .plugin-buttons { display: flex; gap: 4px; align-items: center; flex: none; }
.plugin-review dl { display: grid; grid-template-columns: auto 1fr; gap: 4px 12px; margin: 0; }
.plugin-review dt { color: var(--muted); }
.plugin-review dd { margin: 0; overflow-wrap: anywhere; }
.plugin-review ul { margin: 0; padding-left: 18px; }
`);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];

function icon(path: string) {
  const ns = "http://www.w3.org/2000/svg";
  const s = document.createElementNS(ns, "svg");
  s.setAttribute("viewBox", "0 0 24 24");
  s.setAttribute("aria-hidden", "true");
  const p = document.createElementNS(ns, "path");
  p.setAttribute("d", path);
  s.append(p);
  return s;
}

const say = (text: string, kind = "") => ((note.textContent = text), (note.className = `status ${kind}`));
const originText = (o: Origin) => ("git" in o ? o.git : o.folder);

async function refresh() {
  try {
    installed = (await invoke<Installed[] | null>("plugin_list")) ?? []; // null only outside the app
  } catch (e) {
    installed = [];
    say(String(e), "error");
  }
  render();
}

function render() {
  list.replaceChildren(...installed.map((p) => {
    const m = p.manifest;
    const on = el("input", { type: "checkbox", checked: p.enabled, title: "On" });
    on.onchange = () => void invoke("plugin_enable", { id: p.id, enabled: on.checked }).then(refresh, (e) => say(String(e), "error"));
    const update = el("button", { className: "ghost", textContent: "Update" });
    update.onclick = () => void fetchAndReview(p.origin);
    const remove = el("button", { className: "ghost", textContent: "Remove" });
    remove.onclick = async () => {
      if (!remove.dataset.armed) return void ((remove.dataset.armed = "1"), (remove.textContent = "Remove it?"));
      try {
        await invoke("plugin_remove", { id: p.id });
        say(`Removed ${m?.name ?? p.id}.`, "done");
      } catch (e) {
        say(String(e), "error");
      }
      await refresh();
    };
    const sub = m
      ? `${m.version}${m.author ? ` · ${m.author}` : ""} · ${m.actions.length} action${m.actions.length === 1 ? "" : "s"}${p.granted.length ? ` · may ${p.granted.join(", ")}` : ""}`
      : p.problem ?? "can't be read";
    return el("li", {},
      el("span", { className: "grow" },
        el("span", { textContent: m?.name ?? p.id }),
        el("span", { className: "sub", textContent: sub }),
        ...(m?.description ? [el("span", { className: "sub", textContent: m.description })] : []),
        el("code", { textContent: `${originText(p.origin)}${p.commit ? ` @ ${p.commit.slice(0, 7)}` : ""}` })),
      el("span", { className: "plugin-buttons" }, on, update, remove));
  }));
  if (!installed.length) list.append(el("li", { className: "muted", textContent: "No plugins installed." }));
}

/** Fetches a plugin and shows what it is and asks for, with each permission to tick; installs on the user's word. */
async function fetchAndReview(origin: Origin) {
  say("Fetching…", "busy");
  let staged: Staged;
  try {
    staged = await invoke<Staged>("plugin_fetch", { origin });
    say("");
  } catch (e) {
    return say(String(e), "error");
  }
  const m = staged.manifest;
  const boxes = m.permissions.map((perm) => {
    const box = el("input", { type: "checkbox", checked: staged.granted?.includes(perm) ?? false });
    return { perm, box, row: el("label", { className: "check" }, box, ` ${PERMISSIONS[perm] ?? perm}`) };
  });
  const body = el("div", { className: "plugin-review" },
    el("dl", {},
      el("dt", { textContent: "Plugin" }), el("dd", { textContent: `${m.name} ${m.version}` }),
      ...(m.author ? [el("dt", { textContent: "By" }), el("dd", { textContent: m.author })] : []),
      ...(m.description ? [el("dt", { textContent: "About" }), el("dd", { textContent: m.description })] : []),
      el("dt", { textContent: "From" }), el("dd", { textContent: `${originText(origin)}${staged.commit ? ` @ ${staged.commit.slice(0, 7)}` : ""}` }),
      el("dt", { textContent: "Adds" }), el("dd", {}, el("ul", {}, ...m.actions.map((a) => el("li", { textContent: `${a.title} (${WHERE[a.on]})` })))),
      el("dt", { textContent: "Asks for" }), el("dd", {}, ...(boxes.length ? boxes.map((b) => b.row) : [el("span", { textContent: "Nothing beyond its own sandbox" })]))),
    el("p", { className: "hint", textContent: "Install only plugins you trust. A permission left unticked isn't granted; the plugin's actions that need it will fail." }));
  const { d, footer, setStatus } = modal(staged.granted ? "Update plugin" : "Install plugin", "plugin-review-dialog", body);
  const go = el("button", { className: "primary", textContent: staged.granted ? "Update" : "Install" });
  go.onclick = async () => {
    try {
      const name = await invoke<string>("plugin_install", { token: staged.token, grant: boxes.filter((b) => b.box.checked).map((b) => b.perm) });
      d.close();
      say(`${staged.granted ? "Updated" : "Installed"} ${name}.`, "done");
      gitUrl.value = "";
      await refresh();
    } catch (e) {
      setStatus(String(e), "error");
    }
  };
  const cancel = el("button", { textContent: "Cancel" });
  cancel.onclick = () => d.close();
  footer.append(el("span", { className: "spacer" }), cancel, go);
  d.showModal();
}

fromGit.onclick = () => gitUrl.value.trim() && void fetchAndReview({ git: gitUrl.value.trim() });
gitUrl.onkeydown = (e) => void (e.key === "Enter" && fromGit.click()); // false would cancel typing
fromFolder.onclick = async () => {
  const folder = await openDialog({ directory: true }).catch(() => null);
  if (typeof folder === "string") await fetchAndReview({ folder });
};
tab.addEventListener("click", () => void refresh());

// ---------- Running actions ----------

async function run(p: Installed, a: Action, input: unknown) {
  const c = ctx.conn();
  try {
    const out = await invoke<Output>("plugin_run", { id: p.id, action: a.id, input, connection: c?.name ?? null });
    if (out.clipboard !== null) ctx.copy(out.clipboard, a.title);
    if (out.insert !== null) ctx.insert(out.insert);
    if (out.message !== null) ctx.say(out.message, "done");
    if (out.clipboard === null && out.insert === null && out.message === null) ctx.say(`${a.title}: done`, "done");
  } catch (e) {
    ctx.say(`${a.title}: ${String(e)}`, "error");
  }
}

/** The enabled plugins' actions for `on`, as menu items after a separator; none when there are none. */
export function pluginMenu(on: Target, input: () => unknown): MenuItem[] {
  const items: MenuItem[] = installed.flatMap((p) => (p.enabled && p.manifest ? p.manifest.actions.filter((a) => a.on === on).map((a): MenuItem => [a.title, () => void run(p, a, input())]) : []));
  return items.length ? ["-", ...items] : [];
}

export function initPlugins(c: PluginCtx) {
  ctx = c;
  void refresh();
}
