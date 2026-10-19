// The launcher's list of saved connections: Recent, the groups and the ungrouped ones, with each row's menu and
// dragging rows onto groups. connections.ts keeps the groups and tags; connform.ts edits a connection.

import { invoke } from "../ipc";
import { $, el, setStatus } from "../dom";
import { avatar, dumps, engine, engineId, hasProcesses, hostOf, type Saved } from "./engines";
import * as launcher from "./connections";
import { COLORS, GROUP_ICONS, tagColor, tagLabel } from "./connections";
import { settings } from "../shell/settings";
import { confirmDialog, contextMenu, type MenuItem } from "../shell/dialogs";
import { importFromUrl, openEditor, openPicker } from "./connform";
import { connect } from "../workspace/workspaces";
import { withVault } from "../shell/vault";
import { openBackup, openRestore } from "../database/backup";
import { openProcesses } from "../database/processes";
import { setRecent } from "../shell/menu";

export let saved: Saved[] = [];

function tagPill(tag: string) {
  if (!tag) return "";
  const pill = el("span", { className: `tag ${tag}`, textContent: tagLabel(tag) });
  const color = tagColor(tag);
  if (color) pill.classList.add("colored"), pill.style.setProperty("--tag", color);
  return pill;
}

/** The connection's status color as a small dot; "" when it has none. */
export function colorDot(c: Saved) {
  const color = c.color && COLORS[c.color];
  if (!color) return "";
  const dot = el("span", { className: "color-dot", title: `${c.color} connection` });
  dot.style.background = color;
  return dot;
}

export async function refresh() {
  try {
    const all = await invoke<launcher.Launcher<Saved>>("launcher");
    saved = all.connections;
    launcher.setLauncher(all);
    setStatus("home-status", "");
  } catch (e) {
    setStatus("home-status", String(e), "error"); // e.g. an unreadable connections file, which is left untouched
  }
  renderConns();
}

/** Reloads the list, then says what happened (refresh clears the status line). */
export const homeNote = async (text: string, error = false) => (await refresh(), setStatus("home-status", text, error ? "error" : "done"));
const collapsed = new Set<string>(); // group names folded in the launcher, for this session

/** The launcher list: Recent first (when not searching), then each group, then the ungrouped connections. */
export function renderConns() {
  const q = $<HTMLInputElement>("search").value.toLowerCase();
  const shown = saved.filter((c) => [c.name, c.kind, c.tag, c.group ?? "", hostOf(c)].join(" ").toLowerCase().includes(q));
  const by = settings.launcherSort;
  const key = (c: Saved) => (by === "kind" ? engine(engineId(c)).label : by === "tag" ? c.tag || "\uffff" : "");
  shown.sort((a, b) => key(a).localeCompare(key(b)) || a.name.localeCompare(b.name));
  $("no-conns").hidden = saved.length > 0;
  const sections: HTMLElement[] = [];
  const recent = q ? [] : launcher.recent().flatMap((n) => saved.filter((c) => c.name === n)).slice(0, 5);
  if (recent.length && saved.length > 5) sections.push(section({ name: "", icon: "" }, "Recent", recent, false));
  for (const g of launcher.groups) sections.push(section(g, g.name, shown.filter((c) => c.group === g.name), true));
  const loose = shown.filter((c) => !c.group || !launcher.groups.some((g) => g.name === c.group));
  if (loose.length || launcher.groups.length) sections.push(section({ name: "", icon: "" }, launcher.groups.length ? "Ungrouped" : "", loose, true));
  $("conns").replaceChildren(...sections);
}

/** One heading (a group's is a drop target and has its own menu) and its connections. */
function section(g: launcher.Group, title: string, conns: Saved[], dropTarget: boolean) {
  const items = collapsed.has(title) ? [] : conns.map(connRow);
  const out = el("li", { className: "conn-section" });
  if (title) {
    const head = el("div", { className: "conn-group", tabIndex: 0 },
      el("span", { className: "fold", textContent: collapsed.has(title) ? "▸" : "▾" }),
      g.icon ? el("span", { textContent: GROUP_ICONS[g.icon] ?? "" }) : "",
      el("span", { className: "group-name", textContent: title }),
      el("span", { className: "count", textContent: String(conns.length) }));
    const fold = () => (collapsed.has(title) ? collapsed.delete(title) : collapsed.add(title), renderConns());
    head.onclick = fold;
    head.onkeydown = (e) => void (e.key === "Enter" && fold()); // returning false would cancel Tab
    if (g.name) head.oncontextmenu = (e) => contextMenu(e, groupMenu(g));
    if (dropTarget) head.dataset.group = g.name;
    out.append(head);
  }
  out.append(el("ul", { className: "conns" }, ...items));
  return out;
}

function groupMenu(g: launcher.Group): MenuItem[] {
  return [
    ["Edit Group…", async () => (await launcher.editGroup(g)) && refresh()],
    ["Export Group…", () => launcher.exportConnections({ kind: "group", name: g.name }, `the group “${g.name}”`, homeNote)],
    "-",
    ["Delete Group", async () => {
      if (!(await confirmDialog(`Delete the group "${g.name}"? Its connections stay, ungrouped.`, "Delete"))) return;
      await invoke("delete_group", { name: g.name }).catch((e) => homeNote(String(e), true));
      refresh();
    }],
  ];
}

/** Marks a discovered connection whose server refused the default sign-in; clicking it opens the form for the credentials. */
function credentialsBadge(c: Saved) {
  if (!c.needs_credentials) return "";
  const badge = el("button", { className: "badge-credentials", textContent: "Needs credentials", title: "The server refused the default sign-in. Enter its user and password." });
  badge.onclick = (e) => (e.stopPropagation(), openEditor(engineId(c), c));
  badge.ondblclick = (e) => e.stopPropagation();
  return badge;
}

function connRow(c: Saved) {
  const edit = el("button", { className: "icon", textContent: "Edit", title: `Edit ${c.name}` });
  const del = el("button", { className: "icon", textContent: "Delete", title: `Delete ${c.name}` });
  edit.onclick = (e) => (e.stopPropagation(), openEditor(engineId(c), c));
  del.onclick = (e) => (e.stopPropagation(), deleteConn(c));
  const li = el(
    "li",
    { tabIndex: 0 },
    avatar(engineId(c)),
    el("div", { className: "who" }, el("div", { className: "name" }, el("span", { textContent: c.name }), tagPill(c.tag), credentialsBadge(c)), el("div", { className: "host", textContent: hostOf(c) })),
    el("div", { className: "actions" }, edit, del),
  );
  if (c.color) li.style.setProperty("--conn-color", COLORS[c.color] ?? "transparent"), li.classList.add("colored");
  li.ondblclick = () => !dragged && connect(c);
  li.oncontextmenu = (e) => contextMenu(e, connMenu(c));
  li.onkeydown = (e) => void (e.key === "Enter" && connect(c));
  li.onpointerdown = (e) => e.button === 0 && dragToGroup(e, c);
  return li;
}

async function deleteConn(c: Saved) {
  if (!(await confirmDialog(`Delete "${c.name}"? Its keychain password is removed too.`, "Delete"))) return;
  await invoke("delete_connection", { name: c.name });
  refresh();
}

const moveTo = (c: Saved, group: string) => invoke("move_to_group", { names: [c.name], group }).then(refresh, (e) => homeNote(String(e), true));

/** A launcher row's right-click: connection actions, Move to group, then the tools. */
function connMenu(c: Saved): MenuItem[] {
  const groupItems: MenuItem[] = launcher.groups.filter((g) => g.name !== c.group).map((g) => [`Move to “${g.name}”`, () => moveTo(c, g.name)]);
  if (c.group) groupItems.push(["Remove from Group", () => moveTo(c, "")]);
  return [
    ["Connect", () => connect(c)],
    ["Edit…", () => openEditor(engineId(c), c)],
    ["Duplicate", () => withVault(() => invoke<string>("duplicate_connection", { name: c.name })).then((n) => homeNote(`Duplicated as “${n}”.`), (e) => homeNote(String(e), true))],
    ["Copy as URL", () => navigator.clipboard.writeText(launcher.connectionUrl(c)).then(() => homeNote("Copied the URL (without the password).")).catch((e) => homeNote(String(e), true))],
    ["Export…", () => launcher.exportConnections({ kind: "connection", name: c.name }, `“${c.name}”`, homeNote)],
    ...(groupItems.length ? ["-" as const, ...groupItems] : []),
    "-",
    ...toolsMenu(c),
    "-",
    ["Delete…", () => deleteConn(c)],
  ];
}

let dragged = false; // set while a drag ends, so the row's click doesn't connect

/** Drags a row onto a group heading (or Ungrouped) to move it there. Pointer events rather than HTML drag and drop,
 *  which the app window's own file drop handling gets in the way of. */
function dragToGroup(start: PointerEvent, c: Saved) {
  let ghost: HTMLElement | null = null;
  let over: HTMLElement | null = null;
  const move = (e: PointerEvent) => {
    if (!ghost && Math.hypot(e.clientX - start.clientX, e.clientY - start.clientY) < 6) return;
    ghost ??= document.body.appendChild(el("div", { className: "drag-ghost", textContent: c.name }));
    Object.assign(ghost.style, { left: `${e.clientX + 8}px`, top: `${e.clientY + 8}px` });
    over?.classList.remove("drop");
    over = (document.elementFromPoint(e.clientX, e.clientY) as HTMLElement | null)?.closest<HTMLElement>("[data-group]") ?? null;
    over?.classList.add("drop");
  };
  const up = () => {
    removeEventListener("pointermove", move);
    removeEventListener("pointerup", up);
    if (!ghost) return;
    ghost.remove();
    over?.classList.remove("drop");
    dragged = true;
    setTimeout(() => (dragged = false));
    if (over && over.dataset.group !== (c.group ?? "")) moveTo(c, over.dataset.group!);
  };
  addEventListener("pointermove", move);
  addEventListener("pointerup", up);
}

/** The launcher's ⋯ menu. */
function launcherMenu(): MenuItem[] {
  return [
    ["New Connection…", openPicker],
    ["New Group…", async () => (await launcher.editGroup()) && refresh()],
    ["Manage Tags…", () => launcher.manageTags(refresh)],
    "-",
    ["Find Local Databases…", findLocal],
    ["Import from URL…", importFromUrl],
    ["Import Connections…", () => launcher.importConnections(homeNote)],
    ["Export All Connections…", () => launcher.exportConnections({ kind: "all" }, "all connections", homeNote)],
  ];
}

export function findLocal() {
  setStatus("home-status", "Looking for local databases…", "busy");
  return launcher.discoverLocal(settings.safeLevel, homeNote);
}

/** Backup, restore and the process list, on a launcher row's right-click and the workspace's Tools button. */
export function toolsMenu(c: Saved, database?: string | null): MenuItem[] {
  const items: MenuItem[] = dumps(c) ? [["Backup…", () => openBackup(c, database)], ["Restore…", () => openRestore(c, database, confirmDialog)]] : [];
  if (hasProcesses(c)) items.push(...(items.length ? ["-" as const] : []), ["Processes", () => openProcesses(c, confirmDialog)]);
  return items;
}

/** File › Open Recent. */
export function showRecent() {
  setRecent(launcher.recent().flatMap((n) => saved.filter((c) => c.name === n)).map((c) => ({ label: c.name, run: () => connect(c) })));
}

export function initHome() {
  $("launcher-menu").onclick = () => {
    const r = $("launcher-menu").getBoundingClientRect();
    contextMenu(new MouseEvent("click", { clientX: r.left, clientY: r.bottom + 4 }), launcherMenu());
  };

  $("create").onclick = openPicker;
  $("add").onclick = openPicker;
  $("search").addEventListener("input", renderConns);
  $("search").addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    const first = $("conns").querySelector("li:not(.conn-section)");
    if (first) (first as HTMLElement).click();
  });
}
