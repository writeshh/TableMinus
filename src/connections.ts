// The launcher's connection management: groups, tags and their colors, status colors, connection URLs (import and
// Copy as URL), exporting and importing connections as a file, "ask every time" passwords and the Open Recent list.
// Everything lands in the connections file through launcher.rs; main.ts draws the list.

import { codeOf, invoke } from "./ipc";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { el } from "./dom";
import { readPref, writePref } from "./prefs";
import { withVault } from "./vault";

export type Group = { name: string; icon: string };
export type Tag = { name: string; color: string };
export type Launcher<S> = { connections: S[]; groups: Group[]; tags: Tag[] };

export let groups: Group[] = [];
export let tags: Tag[] = [];
export function setLauncher(d: { groups: Group[]; tags: Tag[] }) {
  ({ groups, tags } = d);
}

/** The five status colors, as the backend names them. */
export const COLORS: Record<string, string> = { red: "#e5484d", orange: "#f76b15", green: "#30a46c", blue: "#0090ff", purple: "#8e4ec6" };
export const GROUP_ICONS: Record<string, string> = { folder: "📁", server: "🖥️", cloud: "☁️", laptop: "💻", star: "⭐", lock: "🔒",
  database: "🗄️", rocket: "🚀", flask: "🧪", bug: "🐛", briefcase: "💼", home: "🏠", globe: "🌐", fire: "🔥", bolt: "⚡", chart: "📊", gear: "⚙️", shield: "🛡️" };
const BUILT_IN_TAGS: Record<string, string> = { local: "Local", development: "Development", staging: "Staging", production: "Production" };
export const tagLabel = (name: string) => BUILT_IN_TAGS[name] ?? name;
export const tagColor = (name: string) => tags.find((t) => t.name === name)?.color;

// ---------- Small dialogs: a text or password prompt ----------

const promptInput = el("input", { autocomplete: "off" });
const promptText = el("p");
const promptError = el("p", { className: "status error" });
const promptCancel = el("button", { type: "button", textContent: "Cancel" });
const promptOk = el("button", { className: "primary" });
const promptTitle = el("h3");
const promptForm = el("form", { method: "dialog", className: "prompt-form" }, promptText, promptInput, promptError,
  el("footer", {}, promptCancel, el("span", { className: "spacer" }), promptOk));
const promptDialog = el("dialog", { className: "small" }, promptTitle, promptForm);
document.body.append(promptDialog);

/** Asks for one line of text (a password when `secret`); null when cancelled. */
export function prompt(o: { title: string; text: string; ok: string; secret?: boolean; value?: string; error?: string; placeholder?: string }): Promise<string | null> {
  promptTitle.textContent = o.title;
  promptText.textContent = o.text;
  promptError.textContent = o.error ?? "";
  promptOk.textContent = o.ok;
  promptInput.type = o.secret ? "password" : "text";
  promptInput.value = o.value ?? "";
  promptInput.placeholder = o.placeholder ?? "";
  promptDialog.showModal();
  promptInput.select();
  return new Promise((resolve) => {
    let result: string | null = null;
    promptForm.onsubmit = (e) => (e.preventDefault(), (result = promptInput.value), promptDialog.close());
    promptCancel.onclick = () => promptDialog.close();
    promptDialog.onclose = () => ((promptInput.value = ""), resolve(result)); // a password doesn't stay in the DOM
  });
}

// ---------- "Ask every time" passwords ----------

/** Runs `send` (a connect); when the connection asks for its password every time, asks, hands it to the backend for
 *  this session, and sends again. A wrong password is forgotten, so the next try asks again. Null when cancelled. */
export async function withAskedPassword<T>(name: string, send: () => Promise<T>): Promise<T | null> {
  try {
    return await send();
  } catch (e) {
    if (codeOf(e) !== "ask_password") throw e;
  }
  const password = await prompt({ title: "Password", text: `Enter the database password of “${name}”. It's kept until you close the workspace.`, ok: "Connect", secret: true });
  if (password === null) return null;
  await invoke("set_session_password", { name, password });
  try {
    return await send();
  } catch (e) {
    forgetAskedPassword(name);
    throw e;
  }
}
export const forgetAskedPassword = (name: string) => void invoke("set_session_password", { name, password: null }).catch(() => {});

// ---------- Groups ----------

const groupName = el("input", { autocomplete: "off", placeholder: "Group name" });
const groupIcons = el("div", { className: "icon-pick" });
const groupError = el("p", { className: "status error" });
const groupCancel = el("button", { type: "button", textContent: "Cancel" });
const groupTitle = el("h3");
const groupForm = el("form", { method: "dialog" }, el("label", {}, "Name", groupName), el("div", { className: "opt-row" }, el("span", { textContent: "Icon" }), groupIcons),
  groupError, el("footer", {}, groupCancel, el("span", { className: "spacer" }), el("button", { className: "primary", textContent: "Save" })));
const groupDialog = el("dialog", { className: "small" }, groupTitle, groupForm);
document.body.append(groupDialog);

/** New group, or edit `existing`; resolves true once saved. */
export function editGroup(existing?: Group): Promise<boolean> {
  let icon = existing?.icon ?? "folder";
  const drawIcons = () =>
    groupIcons.replaceChildren(...Object.entries(GROUP_ICONS).map(([k, glyph]) => {
      const b = el("button", { type: "button", textContent: glyph, title: k });
      b.setAttribute("aria-pressed", String(k === icon));
      b.onclick = () => ((icon = k), drawIcons());
      return b;
    }));
  drawIcons();
  groupTitle.textContent = existing ? "Edit group" : "New group";
  groupName.value = existing?.name ?? "";
  groupError.textContent = "";
  groupDialog.showModal();
  groupName.focus();
  return new Promise((resolve) => {
    groupForm.onsubmit = async (e) => {
      e.preventDefault();
      try {
        await invoke("save_group", { group: { name: groupName.value.trim(), icon }, previous: existing?.name ?? null });
        groupDialog.close();
        resolve(true);
      } catch (err) {
        groupError.textContent = String(err);
      }
    };
    groupCancel.onclick = () => groupDialog.close();
    groupDialog.onclose = () => resolve(false);
  });
}

// ---------- Tag manager ----------

const tagList = el("div", { className: "tag-list" });
const tagError = el("p", { className: "status error" });
const tagAdd = el("button", { type: "button", textContent: "Add tag" });
const tagDone = el("button", { type: "button", className: "primary", textContent: "Done" });
const tagDialog = el("dialog", { className: "small" }, el("h3", { textContent: "Tags" }), tagList, tagError,
  el("footer", {}, tagAdd, el("span", { className: "spacer" }), tagDone));
document.body.append(tagDialog);

/** Edits tags in place: each change is saved as it's made. `changed` reloads the launcher's data. */
export function manageTags(changed: () => Promise<void>) {
  const save = async (tag: Tag, previous: string | null) => {
    try {
      await invoke("save_tag", { tag, previous });
      tagError.textContent = "";
      await changed();
    } catch (e) {
      tagError.textContent = String(e);
    }
    draw();
  };
  const draw = () =>
    tagList.replaceChildren(...tags.map((t) => {
      const color = el("input", { type: "color", value: t.color, title: "Color" });
      const name = el("input", { value: t.name, autocomplete: "off", title: tagLabel(t.name) });
      const del = el("button", { type: "button", className: "icon", textContent: "Delete" });
      color.onchange = () => save({ name: t.name, color: color.value }, t.name);
      name.onchange = () => save({ name: name.value.trim(), color: t.color }, t.name);
      del.onclick = async () => {
        await invoke("delete_tag", { name: t.name }).catch((e) => (tagError.textContent = String(e)));
        await changed();
        draw();
      };
      return el("div", { className: "tag-row" }, color, name, del);
    }));
  tagAdd.onclick = () => {
    let n = 1;
    while (tags.some((t) => t.name === `tag ${n}`)) n++;
    save({ name: `tag ${n}`, color: "#8b8d98" }, null);
  };
  tagDone.onclick = () => tagDialog.close();
  tagError.textContent = "";
  draw();
  tagDialog.showModal();
}

// ---------- Connection URLs ----------

type UrlConn = { kind: string; url: string; name: string; password: string; tls?: string };

// libpq's sslmode values are TLS_MODES' own; MySQL's ssl-mode spells them its way.
const MYSQL_SSL: Record<string, string> = { disabled: "disable", preferred: "prefer", required: "require", verify_ca: "verify-ca", verify_identity: "verify-full" };
const SCHEMES: Record<string, string> = { postgres: "postgres", postgresql: "postgres", mysql: "mysql", mariadb: "mariadb", redis: "redis", rediss: "redis" };

/** A connection from a URL (postgresql://, postgres://, mysql://, mariadb://, redis://, rediss://), or a SQLite path or
 *  sqlite:// URL. */
export function parseConnectionUrl(text: string): UrlConn {
  text = text.trim();
  if (/^(\/|~)/.test(text) || /^sqlite:/i.test(text) || /\.(sqlite3?|db3?)$/i.test(text) && !text.includes("://")) {
    const path = /^sqlite:/i.test(text) ? decodeURIComponent(text.replace(/^sqlite:(\/\/)?/i, "")) : text;
    if (!path) throw "The URL has no file path.";
    return { kind: "sqlite", url: path, name: path.split(/[\\/]/).pop() || path, password: "" };
  }
  let u: URL;
  try {
    u = new URL(text);
  } catch {
    throw "That isn't a connection URL, e.g. postgresql://user@localhost:5432/db.";
  }
  const kind = SCHEMES[u.protocol.slice(0, -1).toLowerCase()];
  if (!kind) throw `TableMinus can't open ${u.protocol}// URLs; it opens postgresql://, mysql://, mariadb://, redis:// and SQLite files.`;
  if (!u.hostname) throw "The URL has no host.";
  const db = decodeURIComponent(u.pathname.replace(/^\//, ""));
  const ssl = u.searchParams.get("sslmode") ?? u.searchParams.get("ssl-mode") ?? u.searchParams.get("ssl_mode");
  // rediss:// is Redis over TLS from the first byte.
  const tls = u.protocol === "rediss:" ? "require" : ssl ? (kind === "postgres" ? ssl.toLowerCase() : MYSQL_SSL[ssl.toLowerCase()]) : undefined;
  const scheme = kind === "postgres" ? "postgres" : kind === "redis" ? "redis" : "mysql"; // the backend's URL schemes
  const port = u.port ? `:${u.port}` : "";
  return {
    kind,
    url: `${scheme}://${u.username || kind !== "redis" ? `${u.username}@` : ""}${u.hostname}${port}${db ? "/" + encodeURIComponent(db) : ""}`,
    name: db ? `${u.hostname}/${db}` : u.hostname,
    password: decodeURIComponent(u.password),
    tls,
  };
}

/** The URL to share a connection by; it never holds the password (the saved URL doesn't). */
export function connectionUrl(c: { kind: string; url: string; tls?: { mode: string } }) {
  if (c.kind === "sqlite") return `sqlite://${c.url}`;
  const u = new URL(c.url);
  if (c.kind === "mariadb") return c.url.replace(/^mysql:/, "mariadb:");
  if (c.kind === "postgres" && c.tls && c.tls.mode !== "prefer") u.searchParams.set("sslmode", c.tls.mode);
  return u.toString();
}

// ---------- Export and import as a file ----------

export type Scope = { kind: "all" } | { kind: "group"; name: string } | { kind: "connection"; name: string };
const FILES = [{ name: "TableMinus connections", extensions: ["tableminus"] }, { name: "All files", extensions: ["*"] }];

const exProtect = el("input", { type: "checkbox" });
const exPassword = el("input", { type: "password", autocomplete: "new-password", placeholder: "Password" });
const exConfirm = el("input", { type: "password", autocomplete: "new-password", placeholder: "Again" });
const exInclude = el("input", { type: "checkbox" });
const exWhat = el("p");
const exError = el("p", { className: "status error" });
const exCancel = el("button", { type: "button", textContent: "Cancel" });
const exForm = el("form", { method: "dialog" }, exWhat,
  el("label", { className: "check" }, exProtect, "Protect the file with a password"),
  el("div", { className: "pw-pair" }, exPassword, exConfirm),
  el("label", { className: "check" }, exInclude, "Include passwords (only in a password-protected file)"),
  exError, el("footer", {}, exCancel, el("span", { className: "spacer" }), el("button", { className: "primary", textContent: "Export…" })));
const exDialog = el("dialog", { className: "small" }, el("h3", { textContent: "Export connections" }), exForm);
document.body.append(exDialog);

const syncExport = () => {
  exPassword.disabled = exConfirm.disabled = !exProtect.checked;
  exInclude.disabled = !exProtect.checked;
  if (!exProtect.checked) exInclude.checked = false;
};
exProtect.onchange = syncExport;

/** Asks how to export `scope` (described by `what`), then where; reports through `done`. */
export function exportConnections(scope: Scope, what: string, done: (text: string, error?: boolean) => void) {
  exWhat.textContent = `Export ${what} to a file you can import on another Mac.`;
  exProtect.checked = exInclude.checked = false;
  exPassword.value = exConfirm.value = exError.textContent = "";
  syncExport();
  exDialog.showModal();
  exCancel.onclick = () => exDialog.close();
  exForm.onsubmit = async (e) => {
    e.preventDefault();
    const password = exProtect.checked ? exPassword.value : null;
    if (exProtect.checked && !password) return void (exError.textContent = "Enter a password, or untick the protection.");
    if (password !== null && password !== exConfirm.value) return void (exError.textContent = "The two passwords differ.");
    const path = await saveFileDialog({ defaultPath: "connections.tableminus", filters: FILES });
    if (!path) return;
    try {
      const n = await invoke<number>("export_connections", { path, scope, password, includePasswords: exInclude.checked });
      exDialog.close();
      done(`Exported ${n} connection${n === 1 ? "" : "s"}.`);
    } catch (err) {
      exError.textContent = String(err);
    } finally {
      exPassword.value = exConfirm.value = "";
    }
  };
}

type Scripts = "keep" | "drop";
const scriptsText = el("pre", { className: "mcp-code" });
Object.assign(scriptsText.style, { maxHeight: "40vh", overflow: "auto" });
const scriptsCancel = el("button", { type: "button", textContent: "Cancel" });
const scriptsDrop = el("button", { type: "button", className: "primary", textContent: "Import Without Them" });
const scriptsKeep = el("button", { type: "button", textContent: "Keep Them" });
const scriptsDialog = el("dialog", {},
  el("h3", { textContent: "Startup scripts" }),
  el("p", { textContent: "Keep them only if you trust whoever made this file: they run each time you connect." }),
  scriptsText, el("footer", {}, scriptsCancel, el("span", { className: "spacer" }), scriptsKeep, scriptsDrop));
document.body.append(scriptsDialog);

/** Shows the startup scripts a connections file brings along; null when cancelled. */
function chooseScripts(text: string): Promise<Scripts | null> {
  scriptsText.textContent = text;
  scriptsDialog.showModal();
  scriptsDrop.focus(); // the safe choice for a stray Enter
  return new Promise((resolve) => {
    let result: Scripts | null = null;
    scriptsDrop.onclick = () => ((result = "drop"), scriptsDialog.close());
    scriptsKeep.onclick = () => ((result = "keep"), scriptsDialog.close());
    scriptsCancel.onclick = () => scriptsDialog.close();
    scriptsDialog.onclose = () => resolve(result);
  });
}

/** Picks an exported file and adds its connections, asking for the file's password when it has one, and what to do
 *  with startup scripts when it brings any. */
export async function importConnections(done: (text: string, error?: boolean) => void) {
  const path = await openFileDialog({ multiple: false, directory: false, filters: FILES });
  if (typeof path !== "string") return;
  let password: string | null = null;
  let scripts: Scripts | null = null;
  let error = "";
  for (;;) {
    try {
      const n = await withVault(() => invoke<number>("import_connections", { path, password, scripts }));
      return done(`Imported ${n} connection${n === 1 ? "" : "s"}.`);
    } catch (e) {
      const code = codeOf(e);
      if (code === "import_scripts") {
        scripts = await chooseScripts(String(e));
        if (scripts === null) return;
        continue;
      }
      if (code !== "file_password" && !(password !== null && code === "file_unseal_failed")) return done(String(e), true);
      if (password !== null) error = String(e);
    }
    password = await prompt({ title: "Import connections", text: "This file is password-protected.", ok: "Import", secret: true, error });
    if (password === null) return;
  }
}

// ---------- Local databases found on this computer ----------

type Found = { conn: Record<string, unknown> & { name: string }; detail: string };

const foundList = el("div", { className: "found-list" });
const foundCancel = el("button", { type: "button", textContent: "Cancel" });
const foundSave = el("button", { className: "primary", textContent: "Save" });
const foundDialog = el("dialog", {}, el("h3", { textContent: "Local databases found" }),
  el("p", { textContent: "Tick the ones to save. A server that refused the default sign-in is saved too, marked so you can give its credentials." }),
  el("form", { method: "dialog" }, foundList, el("footer", {}, foundCancel, el("span", { className: "spacer" }), foundSave)));
document.body.append(foundDialog);

/** Asks which of `found` to save; the ones ticked, or null when cancelled. */
function chooseFound(found: Found[]): Promise<Found[] | null> {
  const boxes = found.map(() => el("input", { type: "checkbox", checked: true }));
  foundList.replaceChildren(...found.map((f, i) => el("label", { className: "check found" }, boxes[i],
    el("span", {}, el("strong", { textContent: f.conn.name }),
      el("small", { textContent: f.conn.needs_credentials ? `Needs credentials. ${f.detail}`.trim() : f.detail })))));
  foundDialog.showModal();
  return new Promise((resolve) => {
    let result: Found[] | null = null;
    foundSave.onclick = () => (result = found.filter((_, i) => boxes[i].checked));
    foundCancel.onclick = () => foundDialog.close();
    foundDialog.onclose = () => resolve(result);
  });
}

/** Looks for database servers on this computer, asks which to save, and saves them under free names. New ones get the
 *  default safe mode level `safety`. */
export async function discoverLocal(safety: string, done: (text: string, error?: boolean) => void) {
  try {
    const found = await invoke<Found[]>("discover_local");
    if (!found.length) return done("No new local databases found.");
    const picked = await chooseFound(found);
    if (!picked?.length) return done("");
    const n = await invoke<number>("save_discovered", { found: picked.map((f) => ({ ...f.conn, safety })) });
    done(`Saved ${n} connection${n === 1 ? "" : "s"}.`);
  } catch (e) {
    done(String(e), true);
  }
}

// ---------- Open Recent, and the workspaces to reopen at startup ----------

const RECENT_MAX = 10;
const read = (key: string): string[] => {
  const v = readPref<unknown>(key);
  return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
};
const write = (key: string, v: string[]) => writePref(key, v);

/** Connection names, most recently opened first. */
export const recent = () => read("recent");
export const addRecent = (name: string) => write("recent", [name, ...recent().filter((n) => n !== name)].slice(0, RECENT_MAX));
/** The connections open when the app last quit. */
export const openAtQuit = () => read("openWorkspaces");
export const setOpenAtQuit = (names: string[]) => write("openWorkspaces", names);
