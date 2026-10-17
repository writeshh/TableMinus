// Adding and editing a connection: step 1 picks the engine, step 2 is the form, which tests, saves, or saves and
// connects. A URL or SQLite path (pasted, or opened from Finder or a link) fills a new connection's form.

import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { invoke } from "./ipc";
import { $, el, setStatus } from "./dom";
import * as launcher from "./connections";
import { COLORS, tagLabel } from "./connections";
import { homeNote, refresh, saved } from "./home";
import {
  addressOf, avatar, engine, ENGINES, networkUrl, OPTIONS_DEFAULT, sshTunnel, TLS_DEFAULT, TLS_MODES, tlsSettings,
  type Options, type Saved, type Ssh, type Tls,
} from "./engines";
import { settings } from "./settings";
import { fillLevels, guarded, type Safety } from "./safety";
import { withVault } from "./vault";
import { connect } from "./workspaces";

const form = $<HTMLFormElement>("form");
const field = (name: string) => form.elements.namedItem(name) as HTMLInputElement & HTMLSelectElement;

/** Opens a new connection's form filled from a URL or SQLite path; saving it is up to the user. */
export async function importFromUrl(text?: string) {
  let error = "";
  for (;;) {
    const t = text ?? (await launcher.prompt({
      title: "Import from URL", text: "Paste a connection URL or a SQLite file path.", ok: "Import", error,
      placeholder: "postgresql://user@localhost:5432/database",
    }));
    if (t === null) return;
    try {
      return openFromUrl(launcher.parseConnectionUrl(t));
    } catch (e) {
      if (text !== undefined) return homeNote(String(e), true); // from Finder or a link: nothing to retype
      error = String(e);
    }
  }
}

function openFromUrl(u: ReturnType<typeof launcher.parseConnectionUrl>) {
  const c: Saved = { name: u.name, kind: u.kind, url: u.url, tag: "", safety: settings.safeLevel };
  if (u.tls) c.tls = { ...TLS_DEFAULT, mode: u.tls };
  openEditor(u.kind, c, true, u.password);
}

let picked = ENGINES[0].kind;

export function openPicker() {
  $<HTMLInputElement>("engine-search").value = "";
  renderEngines();
  $<HTMLDialogElement>("picker").showModal();
  $("engine-search").focus();
}

function renderEngines() {
  const q = $<HTMLInputElement>("engine-search").value.toLowerCase();
  const shown = ENGINES.filter((e) => e.label.toLowerCase().includes(q) || e.kind.includes(q));
  if (!shown.some((e) => e.kind === picked) && shown.length) picked = shown[0].kind;
  $("engines").replaceChildren(
    ...shown.map((e) => {
      const b = el("button", { className: "engine", type: "button" }, avatar(e.kind), el("span", { textContent: e.label }));
      b.setAttribute("role", "option");
      b.setAttribute("aria-selected", String(e.kind === picked));
      b.onclick = () => ((picked = e.kind), renderEngines());
      b.ondblclick = () => nextFromPicker();
      return b;
    }),
  );
}

function nextFromPicker() {
  $<HTMLDialogElement>("picker").close();
  openEditor(picked);
}

let editing: Saved | null = null;
let editingKind = "postgres";

/** The form for a new connection, or for editing `existing`; `fresh` fills it from `existing` as a new one. */
export function openEditor(kind: string, existing?: Saved, fresh = false, password = "") {
  const e = engine(kind);
  editing = fresh ? null : existing ?? null;
  editingKind = kind;
  form.reset();
  $("editor-title").textContent = `${e.label} connection`;
  form.querySelector<HTMLElement>(".net")!.hidden = kind === "sqlite";
  form.querySelector<HTMLElement>(".file")!.hidden = kind !== "sqlite";
  setStatus("form-status", "");
  field("via").value = existing?.ssh ? "ssh" : "";
  showSsh();
  const tls = existing?.tls ?? TLS_DEFAULT;
  field("ssl").value = tls.mode;
  field("ssl_ca").value = tls.ca;
  field("ssl_cert").value = tls.cert;
  field("ssl_key").value = tls.key;
  showTlsFiles();
  const options = { ...OPTIONS_DEFAULT, ...existing?.options };
  for (const [k, v] of Object.entries(options)) {
    const input = field(`opt_${k}`);
    if (typeof v === "boolean") input.checked = v;
    else input.value = v;
  }
  for (const node of form.querySelectorAll<HTMLElement>("[data-engines]")) node.hidden = !node.dataset.engines!.split(" ").includes(kind);
  // Open the section when something in it is set, so saved options are never out of sight.
  form.querySelector<HTMLDetailsElement>("details.more")!.open = !!existing?.options;

  const d = e.defaults;
  if (d) {
    for (const k of ["host", "port", "user"] as const) field(k).placeholder = d[k];
    field("database").placeholder = kind === "redis" ? "0 (a number; choose another after connecting)" : "Optional, choose one after connecting";
    if (kind === "redis") field("user").placeholder = "Optional, an ACL user";
  }
  field("name").placeholder = d ? `${e.label} · ${d.host}` : e.label;
  field("password").placeholder = editing ? "Unchanged (in keychain)" : "None";
  field("passphrase").placeholder = editing ? "Unchanged (in keychain), if any" : "Only for SQLCipher-encrypted files";
  field("tag").replaceChildren(el("option", { value: "", textContent: "None" }), ...launcher.tags.map((t) => el("option", { value: t.name, textContent: tagLabel(t.name) })));
  field("group").replaceChildren(el("option", { value: "", textContent: "None" }), ...launcher.groups.map((g) => el("option", { value: g.name, textContent: g.name })));
  field("password_mode").value = existing?.password_mode ?? "keychain";
  field("color").value = existing?.color ?? "";
  if (existing?.tag && !launcher.tags.some((t) => t.name === existing.tag)) field("tag").append(el("option", { value: existing.tag, textContent: existing.tag }));
  showPasswordMode();
  field("ssh_password").placeholder = existing?.ssh ? "Unchanged (in keychain)" : "None";
  field("safety").value = settings.safeLevel; // overwritten below when editing

  if (existing) {
    field("name").value = existing.name;
    field("tag").value = existing.tag;
    field("group").value = existing.group ?? "";
    field(kind === "sqlite" ? "passphrase" : "password").value = password;
    field("safety").value = existing.safety;
    if (existing.ssh) {
      field("ssh_host").value = existing.ssh.host;
      field("ssh_port").value = existing.ssh.port?.toString() ?? "";
      field("ssh_user").value = existing.ssh.user;
      field("ssh_key").value = existing.ssh.key;
    }
    if (kind === "sqlite") field("path").value = existing.url;
    else for (const [k, v] of Object.entries(addressOf(existing.url, d!))) field(k).value = v;
  }
  $<HTMLDialogElement>("editor").showModal();
  field("name").focus();
}

const val = (k: string) => field(k).value.trim() || field(k).placeholder;

/** The password field says where a typed password goes. */
function showPasswordMode() {
  const mode = field("password_mode").value;
  field("password").placeholder = mode === "ask" ? "Asked when connecting (used here for Test)" : mode === "no_password" ? "None" : editing ? "Unchanged (in keychain)" : "None";
}

/** The keychain-stored secret's input: the login password, or for SQLite the SQLCipher passphrase. */
const secretField = () => field(editingKind === "sqlite" ? "passphrase" : "password");

function showTlsFiles() {
  form.querySelector<HTMLElement>(".tls-files")!.hidden = field("ssl").value === "disable";
}

/** The form's options, or undefined when all are defaults. Options for other engines are dropped. */
function formOptions(): Options | undefined {
  const o: Options = { ...OPTIONS_DEFAULT };
  for (const k of Object.keys(OPTIONS_DEFAULT) as (keyof Options)[]) {
    const input = field(`opt_${k}`);
    if (input.closest<HTMLElement>("[data-engines]")?.hidden) continue;
    (o as Record<string, string | boolean>)[k] = typeof OPTIONS_DEFAULT[k] === "boolean" ? input.checked : input.value.trim();
  }
  const isDefault = (Object.keys(o) as (keyof Options)[]).every((k) => o[k] === OPTIONS_DEFAULT[k]);
  return isDefault ? undefined : o;
}

function formTls(): Tls | undefined {
  if (editingKind === "sqlite") return undefined;
  return tlsSettings({ mode: field("ssl").value, ca: field("ssl_ca").value.trim(), cert: field("ssl_cert").value.trim(), key: field("ssl_key").value.trim() });
}

const overSsh = () => editingKind !== "sqlite" && field("via").value === "ssh";

function showSsh() {
  form.querySelector<HTMLElement>(".ssh")!.hidden = !overSsh();
}

function formSsh(): Ssh | undefined {
  if (!overSsh()) return undefined;
  const v = (k: string) => field(`ssh_${k}`).value.trim();
  return sshTunnel({ host: v("host"), port: v("port"), user: v("user"), key: v("key") });
}

function formToSaved(): Saved {
  let url: string;
  if (editingKind === "sqlite") {
    url = field("path").value.trim();
    if (!url) throw "Choose a database file path.";
  } else {
    const v = (k: string) => field(k).value.trim();
    url = networkUrl(editingKind, { host: v("host"), port: v("port"), user: v("user"), database: v("database") });
  }
  const name = val("name");
  if (name !== editing?.name && saved.some((c) => c.name === name)) throw `A connection named "${name}" already exists.`;
  const conn: Saved = { name, kind: editingKind, url, tag: field("tag").value, safety: field("safety").value as Safety };
  // Defaults are left out, as the backend writes them.
  const mode = editingKind === "sqlite" ? "keychain" : (field("password_mode").value as Saved["password_mode"]);
  if (mode !== "keychain") conn.password_mode = mode;
  if (field("color").value) conn.color = field("color").value;
  if (field("group").value) conn.group = field("group").value;
  const ssh = formSsh();
  if (ssh) conn.ssh = ssh; // omitted, not null: the backend refuses unknown or malformed fields
  const tls = formTls();
  if (tls) conn.tls = tls;
  const options = formOptions();
  if (options) conn.options = options;
  return conn;
}

/** Saves the form. Changing a Safe level asks for the password first, as the toolbar lock does. */
async function saveForm() {
  const conn = formToSaved();
  await guarded(editing?.name ?? conn.name, (a) => withVault(() => invoke("save_connection", {
    conn,
    password: conn.password_mode ? null : secretField().value || null,
    sshPassword: field("ssh_password").value || null,
    previous: editing?.name ?? null,
    unlock: a.unlock,
  })));
  return conn;
}

async function guard(fn: () => Promise<unknown>) {
  try {
    await fn();
  } catch (e) {
    setStatus("form-status", String(e), "error");
  }
}

export function initConnForm() {
  $("swatches").replaceChildren(...[["", "None"], ...Object.keys(COLORS).map((k) => [k, k])].map(([value, title]) => {
    const input = el("input", { type: "radio", name: "color", value, title, ariaLabel: title });
    if (value) input.style.setProperty("--swatch", COLORS[value]);
    return input;
  }));
  field("password_mode").addEventListener("change", showPasswordMode);

  field("ssl").replaceChildren(...TLS_MODES.map(([v, label]) => el("option", { value: v, textContent: label })));
  field("ssl").addEventListener("change", showTlsFiles);

  // "Choose…" buttons fill the input named by their data-pick attribute from the native file dialog.
  for (const b of form.querySelectorAll<HTMLButtonElement>("button[data-pick]")) {
    b.onclick = async () => {
      const input = field(b.dataset.pick!);
      const path = await openFileDialog({ multiple: false, directory: false, defaultPath: input.value || undefined });
      if (typeof path === "string") input.value = path;
    };
  }
  field("via").addEventListener("change", () => (showSsh(), overSsh() && field("ssh_host").focus()));

  $("test").onclick = () =>
    guard(async () => {
      const conn = formToSaved();
      setStatus("form-status", "Testing…", "busy");
      const msg = await invoke<string>("test_connection", {
        conn, password: secretField().value, sshPassword: field("ssh_password").value, savedName: editing?.name ?? null,
      });
      setStatus("form-status", msg, "done");
    });

  $("save").onclick = () =>
    guard(async () => {
      await saveForm();
      $<HTMLDialogElement>("editor").close();
      refresh();
    });

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    guard(async () => {
      const conn = await saveForm();
      $<HTMLDialogElement>("editor").close();
      await refresh();
      connect(conn);
    });
  });
  fillLevels(field("safety"));
  $("engine-search").addEventListener("input", renderEngines);
  $("engine-search").addEventListener("keydown", (e) => e.key === "Enter" && (e.preventDefault(), nextFromPicker()));
  $("picker-next").onclick = nextFromPicker;
  $("picker-cancel").onclick = () => $<HTMLDialogElement>("picker").close();
  $("editor-cancel").onclick = () => $<HTMLDialogElement>("editor").close();
}
