// Settings › MCP (server switch, port, approval, access tokens, setup instructions, activity) and the dialog that
// asks the user to allow an agent's query. Imported by main.ts before it wires the settings tabs, so the tab added
// here is wired with the rest.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { $, el } from "./dom";
import { askPassword } from "./safety";

type Token = { id: string; name: string; connections: string[] | null; read_only: boolean; created: number; bridge: boolean };
type Status = {
  enabled: boolean; port: number; running_port: number | null; approval: boolean; tokens: Token[];
  bridge: string; bridge_exists: boolean;
};
type Activity = { at: number; token: string; tool: string; connection: string | null; sql: string | null; outcome: string };
type McpEvent =
  | { kind: "approval"; id: number; token: string; connection: string; sql: string; unlock: boolean }
  | { kind: "approval_closed"; id: number }
  | ({ kind: "activity" } & Activity);

// Static markup only; every value from the backend goes in through textContent.
const PANE = `
<div class="row">
  <span class="k">Server</span>
  <span><label class="check"><input type="checkbox" id="mcp-enabled" /> Let AI agents use DB Manager over MCP</label></span>
</div>
<div class="row">
  <span class="k">Port</span>
  <span><input id="mcp-port" type="number" min="1024" max="65535" class="num-input" /> <span class="hint inline" id="mcp-state"></span></span>
</div>
<div class="row">
  <span class="k">Approval</span>
  <label class="check"><input type="checkbox" id="mcp-approval" /> Ask before an agent runs anything that may change data</label>
</div>
<p class="hint note">The server listens on 127.0.0.1 only, and every request needs an access token. Agents reach open connections, or the ones a token names. A connection in safe mode always asks; read-only tokens never write. Passwords are never shared.</p>
<h4 class="mcp-head">Access tokens <button id="mcp-new" class="ghost">New token…</button></h4>
<div id="mcp-form" class="mcp-box" hidden>
  <div class="row"><span class="k">Name</span><input id="mcp-name" placeholder="e.g. Claude Code" /></div>
  <div class="row"><span class="k">Access</span><select id="mcp-access"><option value="ro">Read-only</option><option value="rw">Read and write</option></select></div>
  <div class="row"><span class="k">Connections</span>
    <span class="checks"><label class="check"><input type="checkbox" id="mcp-all" checked /> Any open connection</label><span id="mcp-conns" class="checks"></span></span>
  </div>
  <footer><span class="spacer"></span><button id="mcp-form-cancel">Cancel</button><button id="mcp-create" class="primary">Create</button></footer>
</div>
<div id="mcp-created" class="mcp-box" hidden>
  <p>Copy the token now; it won't be shown again.</p>
  <code id="mcp-value" class="mcp-code"></code>
  <footer><span class="spacer"></span><button id="mcp-copy-value">Copy</button><button id="mcp-created-done" class="primary">Done</button></footer>
</div>
<ul id="mcp-tokens" class="mcp-list"></ul>
<h4 class="mcp-head">Connect an agent
  <select id="mcp-agent"><option value="claude">Claude Code</option><option value="json">Cursor, Claude Desktop and others</option><option value="http">Over HTTP, with a token</option></select>
</h4>
<pre id="mcp-setup" class="mcp-code"></pre>
<p class="hint" id="mcp-setup-hint"></p>
<h4 class="mcp-head">Activity <span><button id="mcp-refresh" class="ghost">Refresh</button><button id="mcp-clear" class="ghost">Clear</button></span></h4>
<ul id="mcp-activity" class="mcp-list"></ul>
`;

const STYLE = `
section[data-pane="mcp"] { max-height: min(560px, 68vh); overflow-y: auto; }
.mcp-head { display: flex; justify-content: space-between; align-items: center; margin: 8px 0 0; font-size: 13px; }
.mcp-head select { font-weight: normal; }
.mcp-box { display: grid; gap: 10px; padding: 12px; border: 1px solid var(--line); border-radius: 10px; background: var(--raised); }
.mcp-box p { margin: 0; }
.mcp-box footer { margin: 0; }
.mcp-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 2px; }
.mcp-list li { display: flex; gap: 10px; align-items: baseline; padding: 6px 8px; border-radius: 7px; }
.mcp-list li:nth-child(odd) { background: var(--hover); }
.mcp-list .grow { flex: 1; min-width: 0; }
.mcp-list .sub { display: block; color: var(--muted); font-size: 12px; }
.mcp-list code { display: block; font: 11.5px/1.45 var(--mono); white-space: pre-wrap; overflow-wrap: anywhere; }
.mcp-code { margin: 0; padding: 8px 10px; border-radius: 7px; background: var(--hover); font: 11.5px/1.5 var(--mono); white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; }
#mcp-ask pre { max-height: 240px; overflow: auto; }
`;

const sheet = new CSSStyleSheet();
sheet.replaceSync(STYLE);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];

// The tab and its pane, next to the others in the settings dialog.
const settings = $("settings");
const tab = el("button");
tab.setAttribute("role", "tab");
tab.dataset.tab = "mcp";
tab.append(svg("M8 7h8M8 12h8M8 17h5M5 3h14v18H5z"), "MCP");
settings.querySelector(".prefs-tabs")!.append(tab);
const pane = el("section", { hidden: true });
pane.dataset.pane = "mcp";
pane.append(document.createRange().createContextualFragment(PANE));
settings.querySelector("footer")!.before(pane);

function svg(path: string) {
  const ns = "http://www.w3.org/2000/svg";
  const s = document.createElementNS(ns, "svg");
  s.setAttribute("viewBox", "0 0 24 24");
  s.setAttribute("aria-hidden", "true");
  const p = document.createElementNS(ns, "path");
  p.setAttribute("d", path);
  s.append(p);
  return s;
}

let status: Status | null = null;
const input = (id: string) => $<HTMLInputElement>(id);

function say(text: string) {
  $("mcp-state").textContent = text;
}

async function refresh() {
  try {
    status = await invoke<Status>("mcp_status");
  } catch (e) {
    return say(String(e));
  }
  input("mcp-enabled").checked = status.enabled;
  input("mcp-port").value = String(status.port);
  input("mcp-approval").checked = status.approval;
  say(
    status.running_port ? `Running on 127.0.0.1:${status.running_port}${status.running_port !== status.port ? " (preferred port was taken)" : ""}`
    : status.enabled ? "Not running" : "Off",
  );
  renderTokens();
  renderSetup();
  renderActivity(await invoke<Activity[]>("mcp_activity").catch(() => []));
}

async function configure() {
  const port = Math.round(Number(input("mcp-port").value));
  try {
    status = await invoke<Status>("mcp_configure", {
      enabled: input("mcp-enabled").checked, port, approval: input("mcp-approval").checked,
    });
    await refresh();
  } catch (e) {
    say(String(e));
  }
}

function renderTokens() {
  $("mcp-tokens").replaceChildren(
    ...(status?.tokens ?? []).map((t) => {
      const reach = t.connections === null ? "Any open connection" : t.connections.join(", ") || "No connections";
      const access = t.read_only ? "Read-only" : "Read and write";
      const about = t.bridge ? "Used by the dbmanager-mcp bridge; revoking it makes a new one" : `Created ${new Date(t.created).toLocaleDateString()}`;
      const revoke = el("button", { className: "ghost", textContent: "Revoke" });
      revoke.onclick = async () => {
        try {
          await invoke("mcp_revoke_token", { id: t.id });
        } catch (e) {
          say(String(e));
        }
        refresh();
      };
      return el("li", {},
        el("span", { className: "grow" }, el("strong", { textContent: t.name }), el("span", { className: "sub", textContent: `${access} · ${reach} · ${about}` })),
        revoke);
    }),
  );
  if (!status?.tokens.length) $("mcp-tokens").append(el("li", { className: "muted", textContent: "No tokens yet. Turning the server on makes one for the bridge." }));
}

function renderSetup() {
  if (!status) return;
  const bridge = status.bridge;
  const port = status.running_port ?? status.port;
  const which = $<HTMLSelectElement>("mcp-agent").value;
  const quoted = JSON.stringify(bridge);
  $("mcp-setup").textContent =
    which === "claude" ? `claude mcp add dbmanager -- ${quoted}`
    : which === "json" ? JSON.stringify({ mcpServers: { dbmanager: { command: bridge } } }, null, 2)
    : `claude mcp add --transport http dbmanager http://127.0.0.1:${port}/mcp \\\n  --header "Authorization: Bearer YOUR_TOKEN"`;
  $("mcp-setup-hint").textContent =
    which === "http" ? "Any agent that speaks streamable HTTP can connect with a token made above. The URL changes if the preferred port is taken."
    : !status.bridge_exists ? "The bridge isn't next to the app yet: build it with cargo build --bin dbmanager-mcp."
    : "The bridge uses its own token (above). To give an agent less, create a token and add \"env\": {\"DBMANAGER_MCP_TOKEN\": \"…\"} to its entry. Cursor reads ~/.cursor/mcp.json.";
}

function renderActivity(log: Activity[]) {
  $("mcp-activity").replaceChildren(
    ...log.map((a) =>
      el("li", {},
        el("span", { className: "grow" },
          el("span", { className: "sub", textContent: `${new Date(a.at).toLocaleTimeString()} · ${a.token} · ${a.tool}${a.connection ? ` · ${a.connection}` : ""}` }),
          ...(a.sql ? [el("code", { textContent: a.sql })] : []),
          el("span", { textContent: a.outcome }))),
    ),
  );
  if (!log.length) $("mcp-activity").append(el("li", { className: "muted", textContent: "No tool calls yet." }));
}

async function openForm() {
  input("mcp-name").value = "";
  input("mcp-all").checked = true;
  const saved = await invoke<{ name: string }[]>("list_connections").catch(() => []);
  $("mcp-conns").replaceChildren(
    ...saved.map((c) => el("label", { className: "check" }, el("input", { type: "checkbox", value: c.name, disabled: true }), c.name)),
  );
  $("mcp-form").hidden = false;
  $("mcp-created").hidden = true;
  input("mcp-name").focus();
}

async function createToken() {
  const named = [...$("mcp-conns").querySelectorAll<HTMLInputElement>("input:checked")].map((i) => i.value);
  try {
    const value = await invoke<string>("mcp_create_token", {
      name: input("mcp-name").value,
      connections: input("mcp-all").checked ? null : named,
      readOnly: $<HTMLSelectElement>("mcp-access").value === "ro",
    });
    $("mcp-form").hidden = true;
    $("mcp-value").textContent = value;
    $("mcp-created").hidden = false;
    refresh();
  } catch (e) {
    say(String(e));
  }
}

tab.addEventListener("click", refresh);
input("mcp-enabled").onchange = configure;
input("mcp-port").onchange = configure;
input("mcp-approval").onchange = configure;
$("mcp-new").onclick = openForm;
$("mcp-form-cancel").onclick = () => ($("mcp-form").hidden = true);
$("mcp-create").onclick = createToken;
input("mcp-all").onchange = () => {
  for (const i of $("mcp-conns").querySelectorAll<HTMLInputElement>("input")) i.disabled = input("mcp-all").checked;
};
$("mcp-copy-value").onclick = () => navigator.clipboard.writeText($("mcp-value").textContent ?? "");
$("mcp-created-done").onclick = () => {
  $("mcp-value").textContent = ""; // gone from the page once dismissed
  $("mcp-created").hidden = true;
};
$("mcp-agent").onchange = renderSetup;
$("mcp-refresh").onclick = refresh;
$("mcp-clear").onclick = async () => {
  await invoke("mcp_clear_activity").catch(() => {});
  renderActivity([]);
};

// ---------- Approval: one dialog at a time, oldest request first ----------

type Ask = Extract<McpEvent, { kind: "approval" }>;
const queue: Ask[] = [];
const sqlBox = el("pre", { className: "mcp-code" });
const askText = el("p");
const allow = el("button", { className: "primary", textContent: "Allow" });
const deny = el("button", { textContent: "Deny" });
const askDialog = el("dialog", { id: "mcp-ask" },
  el("strong", { textContent: "An AI agent wants to run this query" }), askText, sqlBox,
  el("footer", {}, el("span", { className: "spacer" }), deny, allow));
document.body.append(askDialog);

function showNext() {
  const next = queue[0];
  if (!next) return askDialog.open && askDialog.close();
  askText.textContent = `Token “${next.token}” on ${next.connection}. It isn't provably read-only. Without an answer in a minute it is denied.`;
  sqlBox.textContent = next.sql;
  if (!askDialog.open) askDialog.showModal();
  deny.focus(); // the safe choice for a stray Enter
}

async function answer(ok: boolean) {
  const current = queue[0];
  if (!current) return;
  if (ok && current.unlock) {
    // A Safe level: allowing takes the password, asked again until right or cancelled.
    for (let wrong = false; ; wrong = true) {
      const unlock = await askPassword(current.connection, wrong);
      if (!unlock) return; // back to Allow / Deny
      if (queue[0] !== current) return; // timed out meanwhile
      try {
        await invoke("mcp_answer", { id: current.id, allow: true, unlock });
        break;
      } catch (e) {
        if (e === "WRONG PASSWORD") continue;
        // Allowing failed for another reason (no saved password): it can never be allowed, so deny it.
        invoke("mcp_answer", { id: current.id, allow: false, unlock: null }).catch(() => {});
        break;
      }
    }
  } else invoke("mcp_answer", { id: current.id, allow: ok, unlock: null }).catch(() => {});
  if (queue[0] === current) queue.shift();
  showNext();
}

allow.onclick = () => answer(true);
deny.onclick = () => answer(false);
askDialog.addEventListener("cancel", (e) => (e.preventDefault(), answer(false))); // Esc denies

listen<McpEvent>("mcp", ({ payload: e }) => {
  if (e.kind === "approval") {
    queue.push(e);
    if (queue.length === 1) showNext();
  } else if (e.kind === "approval_closed") {
    const i = queue.findIndex((q) => q.id === e.id);
    if (i >= 0) (queue.splice(i, 1), i === 0 && showNext());
  } else if (!pane.hidden && settings.hasAttribute("open")) {
    invoke<Activity[]>("mcp_activity").then(renderActivity, () => {});
  }
}).catch(() => {}); // outside the app (a plain browser preview) there are no events
