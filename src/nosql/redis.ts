// Redis workspaces (nosql/redis.rs): in place of tables and SQL, the sidebar lists keys (SCAN, with a pattern) and the main
// area shows the chosen key's value by type, with its edits, over a console that runs commands as redis-cli does.
// Every change and every command that may write goes through safe mode, like SQL.

import { $, el } from "../dom";
import { modal } from "../database/backup";
import { CANCELLED, guarded } from "../shell/safety";
import { formatReply, formatTtl, namespaces, type Entry, type Folder, type Key, type Shown } from "./redis-text";

type Scanned = { cursor: string; keys: Entry[] };
type Value = { type: string; ttl_ms: number; size: number; items: string[][] };
type MenuItem = [string, () => void] | "-";
type Edit = Record<string, unknown> & { op: string };

export type RedisCtx = {
  /** The on-screen connection: `name` is the session key commands take. */
  conn: () => { name: string; kind: string } | null;
  call: <T>(cmd: string, args: Record<string, unknown>) => Promise<T>;
  confirm: (text: string, okLabel: string) => Promise<boolean>;
  menu: (e: MouseEvent, items: MenuItem[]) => void;
};
let ctx: RedisCtx;

/** Keys one Load more adds, at most; SCAN steps run until there are this many or the scan ends. */
const BATCH = 500;
const STEPS = 20;
const TYPES: Record<string, [string, string]> = {
  string: ["S", "String"], hash: ["H", "Hash"], list: ["L", "List"], set: ["T", "Set"], zset: ["Z", "Sorted set"], stream: ["X", "Stream"],
};

/** Each Redis workspace's keys, scan position, pattern, chosen key and console. */
type State = { limit: number; pattern: string; cursor: string; keys: Entry[]; loading: boolean; key: Key | null; value: Value | null; problem: string; log: { line: string; out: string; error: boolean }[]; history: string[] };
/** Items of a key's value shown at first, and added by each Show more (nosql/redis.rs reads at most MAX_SHOWN). */
const SHOWN = 1000;
const MAX_SHOWN = 50_000;
const states = new Map<string, State>();
const stateOf = (name: string): State => {
  let s = states.get(name);
  if (!s) states.set(name, (s = { limit: SHOWN, pattern: "", cursor: "0", keys: [], loading: false, key: null, value: null, problem: "", log: [], history: [] }));
  return s;
};
const sameKey = (a: Key | null, b: Key | null) => !!a && !!b && a.text === b.text && (a.hex ?? null) === (b.hex ?? null);

// ---------- DOM ----------

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
#workspace:not(.redis) :is(#redis-side, #redis-main) { display: none; }
body.redis-ws #dbpicker :is(#db-new, #db-rename, #db-drop) { display: none; }
#workspace.redis :is(#side-items, .side-tabs, #side-search, #side-history) { display: none !important; }
#workspace.redis :is(#panes, #filters, #table-foot) { display: none !important; }
body:has(#workspace.redis) :is(#new-sql, #tools-button, #assistant-button, .tool-icons) { display: none !important; }
#redis-side { display: flex; flex-direction: column; gap: 6px; min-height: 0; flex: 1; }
#redis-side .redis-find { display: flex; gap: 4px; }
#redis-side .redis-find input { flex: 1; min-width: 0; }
#redis-keys { list-style: none; margin: 0; padding: 0; overflow: auto; flex: 1; min-height: 0; }
#redis-keys li { display: flex; gap: 6px; align-items: center; padding: 3px 8px; border-radius: 999px; cursor: default; white-space: nowrap; }
#redis-keys li:hover { background: var(--hover); }
#redis-keys li.active { background: color-mix(in srgb, var(--accent) 16%, transparent); }
#redis-keys .name { overflow: hidden; text-overflow: ellipsis; font: 12px/1.4 var(--mono); }
#redis-keys .folder .chevron { width: 18px; text-align: center; color: var(--muted); flex: none; }
#redis-keys .folder .count { margin-left: auto; color: var(--muted); font-size: 11px; }
.redis-type { flex: none; width: 18px; height: 18px; display: inline-grid; place-items: center; border-radius: 5px; font: 700 10px/1 var(--mono); color: var(--ink); background: var(--yellow); }
.redis-type.hash { background: var(--blue); } .redis-type.list { background: var(--orange); } .redis-type.set { background: var(--pink); }
.redis-type.zset { background: color-mix(in srgb, var(--blue) 50%, var(--pink)); } .redis-type.stream { background: var(--muted); }
#redis-side .redis-foot { display: flex; align-items: center; gap: 6px; color: var(--muted); font-size: 12px; }
#redis-main { display: flex; flex-direction: column; min-height: 0; flex: 1; }
#redis-key { flex: 1; min-height: 0; overflow: auto; padding: 12px 16px; display: grid; align-content: start; gap: 10px; }
#redis-key header { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
#redis-key header h3 { margin: 0; font: 600 14px/1.3 var(--mono); overflow-wrap: anywhere; user-select: text; -webkit-user-select: text; }
#redis-key .meta { color: var(--muted); font-size: 12px; }
#redis-key textarea { width: 100%; box-sizing: border-box; min-height: 160px; font: 12px/1.45 var(--mono); padding: 8px; resize: vertical; }
#redis-key table { border-collapse: collapse; font: 12px/1.4 var(--mono); width: 100%; }
#redis-key :is(th, td) { padding: 4px 8px; border-bottom: 1px solid var(--line); text-align: left; vertical-align: top; overflow-wrap: anywhere; user-select: text; -webkit-user-select: text; }
#redis-key td.acts { white-space: nowrap; width: 1%; }
#redis-key td.acts button { padding: 1px 8px; font-size: 12px; }
#redis-key .add { display: flex; gap: 6px; flex-wrap: wrap; }
#redis-key .add input { flex: 1; min-width: 120px; }
#redis-console { flex: none; height: 220px; display: flex; flex-direction: column; border-top: 1px solid var(--line); background: var(--side); }
#redis-console pre { flex: 1; min-height: 0; overflow: auto; margin: 0; padding: 8px 12px; font: 12px/1.45 var(--mono); white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; -webkit-user-select: text; }
#redis-console pre .cmd { color: var(--accent); }
#redis-console pre .err { color: var(--danger); }
#redis-console form { display: flex; gap: 6px; padding: 6px 8px; border-top: 1px solid var(--line); }
#redis-console input { flex: 1; font: 12px/1.4 var(--mono); }
`);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];

const pattern = el("input", { type: "search", placeholder: "Keys, e.g. user:*", title: "A SCAN pattern: * and ? match, [ae] a set" });
const newKey = el("button", { className: "ghost", textContent: "+", title: "New key" });
const reload = el("button", { className: "ghost", textContent: "↻", title: "Scan again" });
const keyList = el("ul", { id: "redis-keys" });
const foot = el("div", { className: "redis-foot" });
const side = el("div", { id: "redis-side" }, el("div", { className: "redis-find" }, pattern, reload, newKey), keyList, foot);
const keyView = el("div", { id: "redis-key" });
const consoleLog = el("pre");
const consoleInput = el("input", { placeholder: "A command, as in redis-cli: HGETALL user:1", spellcheck: false, autocomplete: "off" });
const consoleForm = el("form", {}, consoleInput, el("button", { type: "submit", textContent: "Run" }));
const consolePane = el("section", { id: "redis-console" }, consoleLog, consoleForm);
const main = el("section", { id: "redis-main" }, keyView, consolePane);

const session = () => ctx.conn()?.name ?? "";
const isRedis = () => ctx.conn()?.kind === "redis";

/** Shows the Redis view when the workspace on screen is a Redis one; `fresh` scans again. */
export function syncRedis(fresh = false) {
  const on = isRedis();
  $("workspace").classList.toggle("redis", on);
  document.body.classList.toggle("redis-ws", on); // the database window is outside the workspace
  if (!on) return;
  const s = stateOf(session());
  if (fresh) Object.assign(s, { cursor: "0", keys: [], key: null, value: null, problem: "" });
  pattern.value = s.pattern;
  if (!s.keys.length && !s.loading) void loadMore(true);
  renderKeys();
  renderValue();
  renderConsole();
}

// ---------- Keys ----------

async function loadMore(fromStart = false) {
  const name = session();
  const s = stateOf(name);
  if (s.loading) return;
  if (fromStart) Object.assign(s, { cursor: "0", keys: [] });
  s.loading = true;
  s.problem = "";
  renderKeys();
  try {
    let cursor = s.cursor;
    const found: Entry[] = [];
    for (let step = 0; step < STEPS && found.length < BATCH; step++) {
      const r = await ctx.call<Scanned>("redis_scan", { name, pattern: s.pattern, cursor, count: BATCH });
      found.push(...r.keys);
      cursor = r.cursor;
      if (cursor === "0") break;
    }
    const seen = new Set(s.keys.map((k) => `${k.key.text}\u0000${k.key.hex ?? ""}`));
    s.keys.push(...found.filter((k) => !seen.has(`${k.key.text}\u0000${k.key.hex ?? ""}`)));
    s.keys.sort((a, b) => a.key.text.localeCompare(b.key.text));
    s.cursor = cursor;
  } catch (e) {
    s.problem = String(e);
  } finally {
    s.loading = false;
    if (session() === name) renderKeys();
  }
}

function typeBadge(type: string) {
  const [letter, label] = TYPES[type] ?? ["?", type];
  return el("span", { className: `redis-type ${type}`, textContent: letter, title: label });
}

/** Open folders, by session and path; folders start closed. */
const openFolders = new Map<string, Set<string>>();

function keyRow(k: Entry, s: State, depth: number) {
  const label = depth ? k.key.text.split(":").pop()! : k.key.text;
  const li = el("li", { title: k.key.text }, typeBadge(k.type), el("span", { className: "name", textContent: label || "(empty)" }));
  li.style.paddingLeft = `${8 + depth * 14}px`;
  li.classList.toggle("active", sameKey(k.key, s.key));
  li.onclick = () => void openKey(k.key);
  li.oncontextmenu = (e) => ctx.menu(e, [
    ["Copy name", () => void navigator.clipboard.writeText(k.key.text).catch(() => {})],
    ["Rename…", () => void renameKey(k.key)],
    "-",
    ["Delete…", () => void deleteKey(k.key)],
  ]);
  return li;
}

function folderRows(f: Folder, s: State, open: Set<string>, depth: number): HTMLElement[] {
  const rows: HTMLElement[] = [];
  for (const [name, sub] of [...f.folders].sort(([a], [b]) => a.localeCompare(b))) {
    const isOpen = open.has(sub.path);
    const li = el("li", { className: "folder", title: `${sub.path}:*` },
      el("span", { className: "chevron", textContent: isOpen ? "▾" : "▸" }), el("span", { className: "name", textContent: name || "(empty)" }),
      el("span", { className: "count", textContent: String(sub.count) }));
    li.style.paddingLeft = `${8 + depth * 14}px`;
    li.onclick = () => (isOpen ? open.delete(sub.path) : open.add(sub.path), renderKeys());
    li.oncontextmenu = (e) => ctx.menu(e, [["Show only these keys", () => ((pattern.value = `${sub.path}:*`), pattern.dispatchEvent(new Event("input")))]]);
    rows.push(li);
    if (isOpen) rows.push(...folderRows(sub, s, open, depth + 1));
  }
  rows.push(...f.keys.map((k) => keyRow(k, s, depth)));
  return rows;
}

function renderKeys() {
  const name = session();
  const s = stateOf(name);
  const open = openFolders.get(name) ?? openFolders.set(name, new Set()).get(name)!;
  keyList.replaceChildren(...folderRows(namespaces(s.keys), s, open, 0));
  const more = el("button", { className: "ghost", textContent: "Load more" });
  more.onclick = () => void loadMore();
  foot.replaceChildren(
    el("span", { textContent: s.loading ? "Scanning…" : s.problem || `${s.keys.length.toLocaleString()} key${s.keys.length === 1 ? "" : "s"}${s.cursor !== "0" ? " so far" : ""}` }),
    ...(s.cursor !== "0" && !s.loading ? [more] : []),
  );
  foot.classList.toggle("error", !!s.problem);
}

let patternTimer = 0;
pattern.oninput = () => {
  clearTimeout(patternTimer);
  patternTimer = window.setTimeout(() => {
    stateOf(session()).pattern = pattern.value.trim();
    void loadMore(true);
  }, 300);
};
reload.onclick = () => void loadMore(true);

// ---------- A key's value ----------

async function openKey(key: Key, limit?: number) {
  const name = session();
  const s = stateOf(name);
  // The same key again (Reload, an edit) keeps how much of it was open; another key starts at the first page.
  s.limit = limit ?? (sameKey(s.key, key) ? s.limit : SHOWN);
  s.key = key;
  s.value = null;
  renderKeys();
  renderValue("Loading…");
  try {
    s.value = await ctx.call<Value>("redis_value", { name, key, limit: s.limit });
  } catch (e) {
    s.problem = String(e);
  }
  if (session() === name && sameKey(s.key, key)) renderValue();
}

/** Runs an edit through safe mode, then shows the key again. Returns whether it ran. */
async function edit(op: Edit, what: string): Promise<boolean> {
  const name = session();
  try {
    await guarded(name, (a) => ctx.call("redis_edit", { name, edit: op, confirmed: a.confirmed, unlock: a.unlock }), () => ctx.confirm(what, "Change"));
    return true;
  } catch (e) {
    if (e !== CANCELLED) renderValue(String(e));
    return false;
  }
}

async function refreshKey() {
  const s = stateOf(session());
  if (s.key) await openKey(s.key);
}

/** A dialog with fields; resolves to their values, or null when cancelled. */
function ask(title: string, fields: [string, string, "line" | "text"][], ok: string): Promise<string[] | null> {
  const inputs = fields.map(([label, value, kind]) => {
    const input = kind === "text" ? el("textarea", { value, rows: 6 }) : el("input", { value, spellcheck: false });
    return { label, input };
  });
  const { d, footer } = modal(title, "small redis-ask", ...inputs.map(({ label, input }) => el("label", { className: "stack" }, label, input)));
  return new Promise((resolve) => {
    let result: string[] | null = null;
    const go = el("button", { className: "primary", textContent: ok });
    go.onclick = () => ((result = inputs.map((i) => i.input.value)), d.close());
    const cancel = el("button", { textContent: "Cancel" });
    cancel.onclick = () => d.close();
    footer.append(el("span", { className: "spacer" }), cancel, go);
    d.addEventListener("close", () => resolve(result));
    d.showModal();
    inputs[0]?.input.focus();
  });
}

const button = (text: string, onclick: () => void, className = "") => Object.assign(el("button", { type: "button", textContent: text, className }), { onclick });

function table(head: string[], rows: string[][], actions: (row: string[], i: number) => HTMLElement[]) {
  return el("table", {},
    el("tr", {}, ...head.map((h) => el("th", { textContent: h })), el("th")),
    ...rows.map((r, i) => el("tr", {}, ...r.map((v) => el("td", { textContent: v })), el("td", { className: "acts" }, ...actions(r, i)))));
}

function renderValue(note = "") {
  const s = stateOf(session());
  if (!s.key) {
    keyView.replaceChildren(el("p", { className: "hint", textContent: "Choose a key on the left, or run a command below." }));
    return;
  }
  const key = s.key;
  const v = s.value;
  const k = { key };
  const head = el("header", {}, ...(v ? [typeBadge(v.type)] : []), el("h3", { textContent: key.text }));
  if (v && v.ttl_ms !== -2) {
    head.append(
      el("span", { className: "meta", textContent: `${v.size.toLocaleString()} ${v.type === "string" ? "bytes" : "items"} · ${formatTtl(v.ttl_ms)}` }),
      el("span", { className: "spacer" }),
      button("Expiry…", () => void setExpiry(key, v.ttl_ms), "ghost"),
      button("Rename…", () => void renameKey(key), "ghost"),
      button("Reload", () => void refreshKey(), "ghost"),
      button("Delete…", () => void deleteKey(key), "ghost"),
    );
  }
  const body: HTMLElement[] = [];
  if (note) body.push(el("p", { className: note.startsWith("Loading") ? "hint" : "status error", textContent: note }));
  if (v?.ttl_ms === -2) body.push(el("p", { className: "hint", textContent: "This key is gone: it expired or was deleted." }));
  else if (v) {
    if (v.items.length < v.size && v.type !== "string") {
      const more = s.limit < MAX_SHOWN;
      body.push(el("p", { className: "hint", textContent: `Showing the first ${v.items.length.toLocaleString()} of ${v.size.toLocaleString()}.${more ? "" : " The console reaches the rest."}` }));
      if (more) body.push(button(`Show ${Math.min(SHOWN, MAX_SHOWN - s.limit).toLocaleString()} more`, () => void openKey(key, Math.min(s.limit + SHOWN, MAX_SHOWN)), "ghost"));
    }
    const after = async (ok: boolean) => ok && (await refreshKey());
    switch (v.type) {
      case "string": {
        const text = el("textarea", { value: v.items[0]?.[0] ?? "" });
        const json = (() => { try { return JSON.parse(text.value), true; } catch { return false; } })();
        body.push(text, el("div", { className: "add" },
          ...(json ? [button("Format JSON", () => (text.value = JSON.stringify(JSON.parse(text.value), null, 2)), "ghost")] : []),
          el("span", { className: "spacer" }),
          button("Save", () => void edit({ op: "set_string", ...k, value: text.value }, `Save the new value of ${key.text}?`).then(after), "primary")));
        if (v.size > text.value.length) body.push(el("p", { className: "hint", textContent: "Only the start of a long value shows; saving writes what's here." }));
        break;
      }
      case "hash": {
        body.push(table(["Field", "Value"], v.items, ([field, value]) => [
          button("Edit", () => void ask(`Edit ${field}`, [["Value", value, "text"]], "Save").then((r) => r && edit({ op: "hash_set", ...k, field, value: r[0] }, `Set ${field} of ${key.text}?`).then(after))),
          button("Delete", () => void edit({ op: "hash_delete", ...k, field }, `Delete field ${field} of ${key.text}?`).then(after)),
        ]));
        const f = el("input", { placeholder: "Field" }), val = el("input", { placeholder: "Value" });
        body.push(el("div", { className: "add" }, f, val, button("Add", () => f.value && void edit({ op: "hash_set", ...k, field: f.value, value: val.value }, `Add field ${f.value} to ${key.text}?`).then(after))));
        break;
      }
      case "list": {
        body.push(table(["Index", "Value"], v.items, ([index, value]) => [
          button("Edit", () => void ask(`Edit item ${index}`, [["Value", value, "text"]], "Save").then((r) => r && edit({ op: "list_set", ...k, index: Number(index), value: r[0] }, `Set item ${index} of ${key.text}?`).then(after))),
          button("Remove", () => void edit({ op: "list_remove", ...k, index: Number(index) }, `Remove item ${index} of ${key.text}?`).then(after)),
        ]));
        const val = el("input", { placeholder: "Value" });
        body.push(el("div", { className: "add" }, val,
          button("Add to head", () => void edit({ op: "list_push", ...k, value: val.value, head: true }, `Add to the head of ${key.text}?`).then(after)),
          button("Add to tail", () => void edit({ op: "list_push", ...k, value: val.value, head: false }, `Add to the tail of ${key.text}?`).then(after))));
        break;
      }
      case "set": {
        body.push(table(["Member"], v.items, ([member]) => [button("Remove", () => void edit({ op: "set_remove", ...k, member }, `Remove ${member} from ${key.text}?`).then(after))]));
        const m = el("input", { placeholder: "Member" });
        body.push(el("div", { className: "add" }, m, button("Add", () => m.value && void edit({ op: "set_add", ...k, member: m.value }, `Add ${m.value} to ${key.text}?`).then(after))));
        break;
      }
      case "zset": {
        body.push(table(["Member", "Score"], v.items, ([member, score]) => [
          button("Score…", () => void ask(`Score of ${member}`, [["Score", score, "line"]], "Save").then((r) => r && edit({ op: "zset_add", ...k, member, score: Number(r[0]) }, `Set the score of ${member}?`).then(after))),
          button("Remove", () => void edit({ op: "zset_remove", ...k, member }, `Remove ${member} from ${key.text}?`).then(after)),
        ]));
        const m = el("input", { placeholder: "Member" }), sc = el("input", { placeholder: "Score", inputMode: "decimal" });
        body.push(el("div", { className: "add" }, m, sc, button("Add", () => m.value && void edit({ op: "zset_add", ...k, member: m.value, score: Number(sc.value) }, `Add ${m.value} to ${key.text}?`).then(after))));
        break;
      }
      case "stream": {
        body.push(table(["ID", "Fields"], v.items.map(([id, ...fv]) => [id, fv.reduce<string[]>((out, x, i) => (i % 2 ? (out[out.length - 1] += ` = ${x}`, out) : [...out, x]), []).join("\n")]), () => []));
        body.push(el("div", { className: "add" }, button("Add entry…", () => void ask("New entry", [["Fields, one field=value per line", "", "text"]], "Add").then((r) => {
          if (!r) return;
          const fields = r[0].split("\n").map((l) => l.trim()).filter(Boolean).map((l) => [l.slice(0, Math.max(0, l.indexOf("="))) || l, l.includes("=") ? l.slice(l.indexOf("=") + 1) : ""]);
          void edit({ op: "stream_add", ...k, fields }, `Add an entry to ${key.text}?`).then(after);
        }))));
        break;
      }
      default:
        body.push(el("p", { className: "hint", textContent: `A ${v.type} key has no view here; use the console.` }));
    }
  }
  keyView.replaceChildren(head, ...body);
}

async function setExpiry(key: Key, ttl: number) {
  const r = await ask(`Expiry of ${key.text}`, [["Seconds from now (empty: no expiry)", ttl > 0 ? String(Math.ceil(ttl / 1000)) : "", "line"]], "Save");
  if (!r) return;
  const seconds = r[0].trim() ? Number(r[0]) : null;
  if (seconds !== null && !(Number.isInteger(seconds) && seconds > 0)) return renderValue("An expiry is a whole number of seconds above 0.");
  if (await edit({ op: "expire", key, seconds }, seconds === null ? `Remove the expiry of ${key.text}?` : `Expire ${key.text} in ${seconds} seconds?`)) await refreshKey();
}

async function renameKey(key: Key) {
  const r = await ask(`Rename ${key.text}`, [["New name", key.text, "line"]], "Rename");
  if (!r || !r[0] || r[0] === key.text) return;
  if (!(await edit({ op: "rename", key, to: r[0] }, `Rename ${key.text} to ${r[0]}?`))) return;
  const s = stateOf(session());
  s.key = { text: r[0] };
  await loadMore(true);
  await openKey(s.key);
}

async function deleteKey(key: Key) {
  if (!(await ctx.confirm(`Delete ${key.text}? This can't be undone.`, "Delete"))) return;
  if (!(await edit({ op: "delete", keys: [key] }, `Delete ${key.text}?`))) return;
  const s = stateOf(session());
  s.keys = s.keys.filter((k) => !sameKey(k.key, key));
  if (sameKey(s.key, key)) (s.key = null), (s.value = null);
  renderKeys();
  renderValue();
}

newKey.onclick = async () => {
  const type = el("select", {}, ...Object.entries(TYPES).map(([v, [, label]]) => el("option", { value: v, textContent: label })));
  const name = el("input", { placeholder: "Key name", spellcheck: false });
  const first = el("input", { placeholder: "Field, member or value", spellcheck: false });
  const second = el("input", { placeholder: "Value or score (hash, sorted set, stream)", spellcheck: false });
  const { d, footer, setStatus } = modal("New key", "small redis-ask", el("label", { className: "stack" }, "Type", type), el("label", { className: "stack" }, "Name", name), first, second);
  const create = el("button", { className: "primary", textContent: "Create" });
  create.onclick = async () => {
    const key = { text: name.value };
    if (!key.text) return setStatus("Name the key.", "error");
    try {
      const exists = await ctx.call<Value>("redis_value", { name: session(), key });
      if (exists.ttl_ms !== -2) return setStatus("A key of that name exists.", "error");
    } catch (e) {
      return setStatus(String(e), "error");
    }
    const ops: Record<string, Edit> = {
      string: { op: "set_string", key, value: first.value },
      hash: { op: "hash_set", key, field: first.value, value: second.value },
      list: { op: "list_push", key, value: first.value, head: false },
      set: { op: "set_add", key, member: first.value },
      zset: { op: "zset_add", key, member: first.value, score: Number(second.value || 0) },
      stream: { op: "stream_add", key, fields: [[first.value || "field", second.value]] },
    };
    d.close();
    if (await edit(ops[type.value], `Create ${key.text}?`)) {
      await loadMore(true);
      await openKey(key);
    }
  };
  const cancel = el("button", { textContent: "Cancel" });
  cancel.onclick = () => d.close();
  footer.append(el("span", { className: "spacer" }), cancel, create);
  d.showModal();
  name.focus();
};

// ---------- Console ----------

function renderConsole() {
  const s = stateOf(session());
  consoleLog.replaceChildren(...s.log.flatMap((l) => [el("span", { className: "cmd", textContent: `› ${l.line}\n` }), el("span", { className: l.error ? "err" : "", textContent: `${l.out}\n` })]));
  consoleLog.scrollTop = consoleLog.scrollHeight;
}

let historyAt = -1;
consoleForm.onsubmit = async (e) => {
  e.preventDefault();
  const line = consoleInput.value.trim();
  if (!line) return;
  const name = session();
  const s = stateOf(name);
  s.history = [line, ...s.history.filter((h) => h !== line)].slice(0, 100);
  historyAt = -1;
  consoleInput.value = "";
  try {
    const r = await guarded(name, (a) => ctx.call<Shown>("redis_command", { name, line, confirmed: a.confirmed, unlock: a.unlock }), () => ctx.confirm(`Run this? ${line.length > 200 ? line.slice(0, 200) + "…" : line}`, "Run"));
    s.log.push({ line, out: formatReply(r), error: r.kind === "error" });
  } catch (err) {
    s.log.push({ line, out: err === CANCELLED ? "(not run)" : `(error) ${String(err)}`, error: err !== CANCELLED });
  }
  s.log = s.log.slice(-500);
  if (session() === name) renderConsole();
  if (s.key) await openKey(s.key); // the command may have changed it
};
consoleInput.onkeydown = (e) => {
  const s = stateOf(session());
  if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
  e.preventDefault();
  historyAt = Math.max(-1, Math.min(s.history.length - 1, historyAt + (e.key === "ArrowUp" ? 1 : -1)));
  consoleInput.value = historyAt < 0 ? "" : s.history[historyAt];
};

export function initRedis(c: RedisCtx) {
  ctx = c;
  $("side-items").after(side);
  $("panes").after(main);
}

/** Forgets a closed workspace's keys and console. */
export const forgetRedis = (name: string) => void states.delete(name);
