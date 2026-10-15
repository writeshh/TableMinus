// The AI assistant (ai.rs): Settings › AI, and a chat pane on the right of the workspace that writes SQL with the
// open database's tables and columns as context. Nothing goes to a provider until the user turns the assistant on
// under the notice that says what is sent. The model may ask to run a query; each one waits for the user's Run and
// runs read-only (`ai_run`). Chats are kept per connection in ai-chats.json, next to the query history.

import { invoke } from "@tauri-apps/api/core";
import { $, connName, el } from "./dom";
import { readPref, writePref } from "./prefs";
import { docEntry, setDocEntry } from "./locations";
import { CANCELLED, guarded } from "./safety";
import { withVault } from "./vault";
import { blocks, pendingCall, resultText, schemaText, type Call, type DiagramData, type QueryResult, type Turn } from "./ai-text";

export type Provider = "open_ai" | "anthropic";
type Chat = { id: string; title: string; at: number; turns: Turn[] };
type Reply = { text: string; call: Call | null };

export type AiPrefs = { enabled: boolean; provider: Provider; urls: Record<Provider, string>; models: Record<Provider, string> };
const DEFAULTS: AiPrefs = {
  enabled: false,
  provider: "anthropic",
  urls: { open_ai: "https://api.openai.com/v1", anthropic: "https://api.anthropic.com" },
  models: { open_ai: "", anthropic: "claude-sonnet-5-5" },
};
const PREF = "ai";
const prefs = (): AiPrefs => {
  const kept = readPref<Partial<AiPrefs>>(PREF) ?? {};
  return { ...DEFAULTS, ...kept, urls: { ...DEFAULTS.urls, ...kept.urls }, models: { ...DEFAULTS.models, ...kept.models } };
};

/** Chats kept per connection; the oldest go first. */
const MAX_CHATS = 30;
const DECLINED = "The user chose not to run this query.";

// ---------- Settings › AI ----------

// Static markup only; values go in through value and textContent.
const SETTINGS = `
<p class="hint note" id="ai-notice">The assistant sends your messages, the open database's table and column names and types, and the rows of every query you let it run to the provider chosen below. Turn it on only if that is allowed for your data.</p>
<div class="row"><span class="k">Assistant</span><label class="check"><input type="checkbox" id="ai-enabled" /> Turn on the assistant, and send to the provider what the notice above says</label></div>
<div class="row"><span class="k">Provider</span><select id="ai-provider"><option value="anthropic">Anthropic</option><option value="open_ai">OpenAI-compatible (OpenAI, Ollama, LM Studio…)</option></select></div>
<div class="row"><span class="k">Base URL</span><input id="ai-url" spellcheck="false" /></div>
<div class="row"><span class="k">Model</span><input id="ai-model" spellcheck="false" /></div>
<div class="row"><span class="k">API key</span>
  <span class="ai-key"><input id="ai-key" type="password" autocomplete="off" spellcheck="false" /><button id="ai-key-save">Save</button><button id="ai-key-remove" class="ghost">Remove</button></span>
</div>
<p class="hint note" id="ai-key-state"></p>
<p class="hint note">The key is kept in the system keychain and never shown again. Plain http:// works only for a server on this computer.</p>
`;

const STYLE = `
.ai-key { display: flex; gap: 6px; }
.ai-key input { flex: 1; }
#workspace.with-assistant { grid-template-columns: min(var(--side-w, 248px), 50vw) 1fr min(var(--ai-w, 340px), 45vw); }
#workspace.with-assistant.no-sidebar { grid-template-columns: 1fr min(var(--ai-w, 340px), 45vw); }
#workspace:not(.with-assistant) > :is(#assistant, .ai-edge) { display: none; }
.ai-edge { right: calc(min(var(--ai-w, 340px), 45vw) - 4px); }
#assistant { display: flex; flex-direction: column; min-height: 0; min-width: 0; background: var(--side); color: var(--side-fg); border-left: 1px solid var(--line); }
#assistant > header { display: flex; align-items: center; gap: 6px; padding: 8px 10px; border-bottom: 1px solid var(--line); }
#assistant > header strong { margin-right: auto; }
#assistant > header select { max-width: 140px; }
.ai-log { flex: 1; min-height: 0; overflow: auto; display: flex; flex-direction: column; gap: 10px; padding: 10px; user-select: text; -webkit-user-select: text; }
.ai-log .hint { margin: 0; }
.ai-msg { display: grid; gap: 6px; max-width: 100%; }
.ai-msg p { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.45; }
.ai-msg.user { justify-self: end; max-width: 88%; padding: 6px 10px; border-radius: 10px; background: color-mix(in srgb, var(--accent) 14%, var(--bg)); }
.ai-code { position: relative; display: grid; border: 1px solid var(--line); border-radius: 8px; background: var(--bg); overflow: hidden; }
.ai-code pre { margin: 0; padding: 8px 10px; font: 12px/1.45 var(--mono); white-space: pre-wrap; overflow-wrap: anywhere; max-height: 260px; overflow: auto; }
.ai-code footer { display: flex; gap: 4px; justify-content: flex-end; margin: 0; padding: 4px 6px; border-top: 1px solid var(--line); background: var(--raised); }
.ai-code footer .grow { flex: 1; color: var(--muted); font-size: 12px; align-self: center; }
.ai-code footer button { padding: 2px 8px; font-size: 12px; }
.ai-ask { border-color: var(--warn); }
.ai-table { max-height: 220px; overflow: auto; border-top: 1px solid var(--line); }
.ai-table table { border-collapse: collapse; font: 11.5px/1.4 var(--mono); }
.ai-table :is(th, td) { padding: 2px 8px; border-bottom: 1px solid var(--line); white-space: nowrap; max-width: 220px; overflow: hidden; text-overflow: ellipsis; text-align: left; }
.ai-table .null { color: var(--muted); font-style: italic; }
.ai-error { color: var(--danger); }
.ai-busy { color: var(--muted); font-style: italic; }
#ai-form { display: grid; gap: 6px; padding: 8px 10px 10px; border-top: 1px solid var(--line); }
#ai-input { resize: none; field-sizing: content; min-height: 54px; max-height: 200px; padding: 6px 8px; font: inherit; }
#ai-form footer { display: flex; align-items: center; gap: 6px; margin: 0; }
#ai-form footer .hint { margin: 0 auto 0 0; }
`;

const sheet = new CSSStyleSheet();
sheet.replaceSync(STYLE);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];

const settingsDialog = $("settings");
const settingsTab = el("button");
settingsTab.setAttribute("role", "tab");
settingsTab.dataset.tab = "ai";
settingsTab.append(icon("M12 3l1.8 4.7L18.5 9.5l-4.7 1.8L12 16l-1.8-4.7L5.5 9.5l4.7-1.8zM18 15l.9 2.1L21 18l-2.1.9L18 21l-.9-2.1L15 18l2.1-.9z"), "AI");
settingsDialog.querySelector(".prefs-tabs")!.append(settingsTab);
const settingsPane = el("section", { hidden: true });
settingsPane.dataset.pane = "ai";
settingsPane.append(document.createRange().createContextualFragment(SETTINGS));
settingsDialog.querySelector(":scope > footer")!.before(settingsPane);

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

const field = <T extends HTMLElement = HTMLInputElement>(id: string) => $<T>(id);

/** Fills the AI pane from the kept settings. */
function fillSettings() {
  const p = prefs();
  field("ai-enabled").checked = p.enabled;
  field<HTMLSelectElement>("ai-provider").value = p.provider;
  field("ai-url").value = p.urls[p.provider];
  field("ai-model").value = p.models[p.provider];
  field("ai-model").placeholder = p.provider === "anthropic" ? "claude-sonnet-5-5" : "e.g. gpt-4.1, or llama3.2 on Ollama";
  field("ai-key").value = "";
  field("ai-key").placeholder = p.provider === "anthropic" ? "sk-ant-…" : "Not needed for most local servers";
}

/** Fills the AI pane, and says whether the keychain has a key; only when the pane is opened, as that reads it. */
async function showSettings() {
  fillSettings();
  const p = prefs();
  const state = $("ai-key-state");
  state.textContent = "";
  try {
    state.textContent = (await invoke<boolean>("ai_has_key", { provider: p.provider })) ? "A key is saved." : "No key saved.";
  } catch (e) {
    state.textContent = String(e);
  }
  renderPane();
}

function saveSettings() {
  const p = prefs();
  const provider = field<HTMLSelectElement>("ai-provider").value as Provider;
  if (provider !== p.provider) {
    writePref(PREF, { ...p, provider });
    return void showSettings(); // the other provider's URL, model and key
  }
  p.enabled = field("ai-enabled").checked;
  p.urls[provider] = field("ai-url").value.trim() || DEFAULTS.urls[provider];
  p.models[provider] = field("ai-model").value.trim();
  writePref(PREF, p);
  renderPane();
}
for (const id of ["ai-enabled", "ai-provider", "ai-url", "ai-model"]) $(id).addEventListener("change", saveSettings);
settingsTab.addEventListener("click", () => void showSettings());

async function setKey(key: string) {
  const state = $("ai-key-state");
  try {
    await withVault(() => invoke("ai_set_key", { provider: prefs().provider, key }));
    field("ai-key").value = "";
    state.textContent = key ? "The key is saved." : "The key is removed.";
  } catch (e) {
    state.textContent = String(e);
  }
}
$("ai-key-save").onclick = () => {
  const key = field("ai-key").value.trim();
  if (key) void setKey(key);
};
$("ai-key-remove").onclick = () => void setKey("");

// ---------- The pane ----------

export type AssistantCtx = {
  /** The on-screen connection: `name` is the session key commands take. */
  conn: () => { name: string; kind: string } | null;
  /** The server as the connection info shows it, such as "PostgreSQL 16.2". */
  server: () => string;
  /** The Postgres schema the sidebar shows; null elsewhere. */
  schema: () => string | null;
  call: <T>(cmd: string, args: Record<string, unknown>) => Promise<T>;
  /** Puts SQL into the current SQL tab, or a new one. */
  insert: (sql: string) => void;
  openSettings: () => void;
};
let ctx: AssistantCtx;

const pane = el("section", { id: "assistant", className: "assistant" });
pane.setAttribute("aria-label", "Assistant");
const chatPick = el("select", { title: "Earlier chats" });
const newChat = el("button", { className: "ghost", textContent: "New chat", title: "Start a new chat" });
const log = el("div", { className: "ai-log" });
log.setAttribute("aria-live", "polite");
const input = el("textarea", { id: "ai-input", placeholder: "Ask for a query, or about the data…", rows: 2 });
const send = el("button", { className: "primary", textContent: "Send", type: "submit" });
const form = el("form", { id: "ai-form" });
form.append(input, el("footer", {}, el("span", { className: "hint", textContent: "Enter sends, Shift+Enter starts a new line" }), send));
pane.append(el("header", {}, el("strong", { textContent: "Assistant" }), chatPick, newChat), log, form);

/** The chat on screen, by connection; `busy` while waiting for the provider. */
let chat: Chat | null = null;
let chatOf = "";
let busy = false;
let problem = "";

const conn = () => ctx.conn();
const chatsOf = (name: string) => docEntry<Chat[]>("chats", name) ?? [];

function keep() {
  const c = conn();
  if (!c || !chat?.turns.length) return;
  const name = connName(c.name);
  const rest = chatsOf(name).filter((x) => x.id !== chat!.id);
  setDocEntry("chats", name, [chat, ...rest].slice(0, MAX_CHATS));
}

function fresh(): Chat {
  return { id: crypto.randomUUID(), title: "", at: Date.now(), turns: [] };
}

/** The chat for the on-screen connection: the one shown, or a new one after switching connections. */
function current(): Chat {
  const name = connName(conn()?.name ?? "");
  if (!chat || chatOf !== name) {
    chat = fresh();
    chatOf = name;
    problem = "";
  }
  return chat;
}

export const assistantOpen = () => $("workspace").classList.contains("with-assistant");

export function toggleAssistant() {
  const open = $("workspace").classList.toggle("with-assistant");
  writePref("layout.assistant-open", open);
  document.getElementById("assistant-button")?.setAttribute("aria-pressed", String(open));
  if (open) {
    renderPane();
    input.focus();
  }
}

function renderPicker() {
  const c = conn();
  const list = c ? chatsOf(connName(c.name)) : [];
  const now = current();
  const options = [el("option", { value: now.id, textContent: now.title || "New chat" })];
  for (const x of list) if (x.id !== now.id) options.push(el("option", { value: x.id, textContent: x.title || "Untitled" }));
  chatPick.replaceChildren(...options);
  chatPick.value = now.id;
  chatPick.disabled = options.length < 2;
}

chatPick.onchange = () => {
  const c = conn();
  const found = c && chatsOf(connName(c.name)).find((x) => x.id === chatPick.value);
  if (!found || busy) return;
  chat = structuredClone(found);
  problem = "";
  renderPane();
};
newChat.onclick = () => {
  if (busy) return;
  chat = fresh();
  problem = "";
  renderPane();
  input.focus();
};

/** A code block with buttons: Insert into the editor and Copy. */
function codeBlock(sql: string, extra?: HTMLElement) {
  const insert = el("button", { textContent: "Insert", title: "Insert into the SQL editor" });
  insert.onclick = () => ctx.insert(sql);
  const copy = el("button", { className: "ghost", textContent: "Copy" });
  copy.onclick = () => void navigator.clipboard.writeText(sql).then(() => (copy.textContent = "Copied"));
  return el("div", { className: "ai-code" }, el("pre", { textContent: sql }), el("footer", {}, ...(extra ? [extra] : []), copy, insert));
}

function resultTable(r: QueryResult) {
  const table = el("table");
  table.append(el("tr", {}, ...r.columns.map((c) => el("th", { textContent: c }))));
  for (const row of r.rows) table.append(el("tr", {}, ...row.map((v) => el("td", v === null ? { className: "null", textContent: "NULL" } : { textContent: v, title: v }))));
  return el("div", { className: "ai-table" }, table);
}

/** Results run in this session, by call id, so the log can show them as tables; the saved chat keeps their text. */
const shownResults = new Map<string, QueryResult>();

function renderTurn(t: Turn, pending: Call | null): HTMLElement | null {
  if (t.kind === "user") return el("div", { className: "ai-msg user" }, el("p", { textContent: t.text }));
  if (t.kind === "result") {
    const r = shownResults.get(t.id);
    const label = t.text === DECLINED ? "Not run" : !r ? "Ran" : r.columns.length ? `${r.rows.length} row${r.rows.length === 1 ? "" : "s"}${r.truncated ? ", more not shown" : ""}` : `${r.affected} rows affected`;
    const box = el("div", { className: "ai-code" }, el("footer", {}, el("span", { className: "grow", textContent: label })));
    // A chat reopened later has only the text the model saw, not the rows.
    if (r?.columns.length) box.append(resultTable(r));
    else if (!r && t.text !== DECLINED) box.append(el("pre", { textContent: t.text }));
    return box;
  }
  const msg = el("div", { className: "ai-msg" });
  for (const b of blocks(t.text)) msg.append(b.code ? codeBlock(b.text) : el("p", { textContent: b.text }));
  if (t.call) {
    const waiting = pending?.id === t.call.id;
    const note = el("span", { className: "grow", textContent: waiting ? "The assistant wants to run this:" : "The assistant asked to run this." });
    const box = codeBlock(t.call.sql, note);
    if (waiting) {
      box.classList.add("ai-ask");
      const run = el("button", { className: "primary", textContent: "Run", title: "Run it read-only and show the assistant the rows" });
      const skip = el("button", { textContent: "Don't run" });
      run.onclick = () => void answer(t.call!, true);
      skip.onclick = () => void answer(t.call!, false);
      box.querySelector("footer")!.append(skip, run);
    }
    msg.append(box);
  }
  return msg;
}

/** Redraws the pane for the on-screen connection. */
export function renderPane() {
  if (!ctx) return;
  const p = prefs();
  const c = conn();
  const now = current();
  renderPicker();
  const items: HTMLElement[] = [];
  if (!p.enabled) {
    const open = el("button", { textContent: "Open Settings › AI" });
    open.onclick = () => (ctx.openSettings(), settingsTab.click());
    items.push(el("p", { className: "hint", textContent: "The assistant is off. Turn it on in Settings › AI, which says what it sends to the provider." }), open);
  } else if (!c) items.push(el("p", { className: "hint", textContent: "Connect to a database to use the assistant." }));
  else if (!now.turns.length) items.push(el("p", { className: "hint", textContent: `Ask for a query on ${connName(c.name)}, or about its data. The assistant sees the table and column names; it asks before it runs anything, and runs only reads.` }));
  const pending = pendingCall(now.turns);
  for (const t of now.turns) {
    const node = renderTurn(t, busy ? null : pending);
    if (node) items.push(node);
  }
  if (busy) items.push(el("p", { className: "ai-busy", textContent: "Thinking…" }));
  if (problem) {
    const retry = el("button", { textContent: "Try again" });
    retry.onclick = () => void step();
    items.push(el("p", { className: "ai-error", textContent: problem }), retry);
  }
  log.replaceChildren(...items);
  log.scrollTop = log.scrollHeight;
  const usable = p.enabled && !!c;
  input.disabled = !usable;
  send.disabled = !usable || busy;
}

/** The schema text by session and Postgres schema, fetched once and kept until the table list is reloaded. */
const schemaCache = new Map<string, string>();
export const forgetSchema = () => schemaCache.clear();
async function schemaFor(key: string) {
  const cacheKey = `${key}\u001f${ctx.schema() ?? ""}`;
  const had = schemaCache.get(cacheKey);
  if (had !== undefined) return had;
  const text = schemaText(await ctx.call<DiagramData>("diagram", { name: key, schema: ctx.schema() }));
  schemaCache.set(cacheKey, text);
  return text;
}

/** Sends the chat and adds the model's answer. */
async function step() {
  const c = conn();
  const p = prefs();
  if (!c || !p.enabled || busy) return;
  const now = current();
  busy = true;
  problem = "";
  renderPane();
  try {
    const request = {
      provider: p.provider,
      base_url: p.urls[p.provider],
      model: p.models[p.provider],
      server: ctx.server() || c.kind,
      schema: await schemaFor(c.name),
      turns: now.turns,
    };
    const reply = await invoke<Reply>("ai_chat", { request });
    now.turns.push({ kind: "assistant", text: reply.text, call: reply.call });
    keep();
  } catch (e) {
    problem = String(e);
  } finally {
    busy = false;
    if (chat === now) renderPane();
  }
}

async function ask(text: string) {
  const now = current();
  // A new message is the user's answer to a query still waiting: it isn't run.
  const pending = pendingCall(now.turns);
  if (pending) now.turns.push({ kind: "result", id: pending.id, text: DECLINED });
  now.turns.push({ kind: "user", text });
  if (!now.title) now.title = text.length > 48 ? text.slice(0, 47) + "…" : text;
  keep();
  await step();
}

async function answer(call: Call, run: boolean) {
  const c = conn();
  const now = current();
  if (!c || pendingCall(now.turns)?.id !== call.id) return;
  let text = DECLINED;
  if (run) {
    try {
      const r = await guarded(c.name, (a) => ctx.call<QueryResult>("ai_run", { name: c.name, sql: call.sql, unlock: a.unlock }));
      shownResults.set(call.id, r);
      text = resultText(r);
    } catch (e) {
      if (e === CANCELLED) return; // still waiting for an answer
      text = `The query failed: ${String(e)}`;
    }
  }
  now.turns.push({ kind: "result", id: call.id, text });
  keep();
  await step();
}

form.onsubmit = (e) => {
  e.preventDefault();
  const text = input.value.trim();
  if (!text || busy || send.disabled) return;
  input.value = "";
  void ask(text);
};
input.onkeydown = (e) => {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    form.requestSubmit();
  }
};

export function initAssistant(c: AssistantCtx) {
  ctx = c;
  fillSettings();
  $("workspace").append(pane);
  if (readPref<boolean>("layout.assistant-open") === true) {
    $("workspace").classList.add("with-assistant");
    document.getElementById("assistant-button")?.setAttribute("aria-pressed", "true");
  }
  renderPane();
}
