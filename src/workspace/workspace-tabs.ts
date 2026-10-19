// Workspace tabs: one per open workspace (a connection, or another database of one), across the top, and the session
// key each workspace gets. workspaces.ts keeps the workspaces and swaps them on and off the screen.

import { $, el } from "../dom";
import { keyLabel, S, type Workspace } from "./state";
import { closeWorkspace, newWorkspace, showWorkspace } from "./workspaces";

/** A session key no open workspace uses: the connection's name, else name + "\u001f" + n (see `connName`). */
export function freeKey(name: string) {
  const taken = new Set(S.workspaces.map((w) => w.key));
  let key = name;
  for (let n = 2; taken.has(key); n++) key = `${name}\u001f${n}`;
  return key;
}

let draggedTab: Workspace | null = null;

const titleActions = el("span", { className: "title-actions" });

/** Each tab shows its connection's tag colour, its database (or the connection, without one), a mark when its
 *  connection was lost and × to close; drag one to reorder. The order is also ⌃⌘[ / ⌃⌘]'s. While the launcher shows
 *  (⌘N, or +) it has a tab of its own, like a browser's new tab, so the open workspaces stay one click away. */
export function renderDbTabs() {
  const strip = $("db-tabs");
  strip.hidden = !S.workspaces.length;
  if (strip.hidden) return;
  // Modules add their header buttons at load, so the move waits for the first time the strip shows.
  if (!titleActions.hasChildNodes()) {
    titleActions.append(...$("where").querySelectorAll(":scope > .tool-icons:not(.in-header), :scope > button:not(#db-button)"));
    // Last, after SQL. Looked up in the container: it is not in the page until the strip is filled, so $ can't find it.
    const assistant = titleActions.querySelector("#assistant-button");
    if (assistant) titleActions.append(assistant);
    titleActions.prepend($("pill")); // the connection, database and tab, as TablePlus's title bar shows them
  }
  const home = !$("home").hidden;
  const launcherTab = () => {
    const close = el("span", { className: "close", textContent: "×", title: "Back to the workspace" });
    close.onclick = (e) => (e.stopPropagation(), S.ws && showWorkspace(S.ws));
    const b = el("button", { className: "db-tab new-ws", title: "Connect to open another workspace" },
      el("span", { className: "dot" }), el("span", { className: "label", textContent: "New workspace" }), close);
    b.setAttribute("role", "tab");
    b.setAttribute("aria-selected", "true");
    return b;
  };
  const add = el("button", { className: "add", textContent: "+", title: `New workspace (${keyLabel("new-workspace")})` });
  add.onclick = newWorkspace;
  strip.replaceChildren(
    ...S.workspaces.map((w) => {
      const database = ((d) => d && (w.conn.kind === "redis" ? `db${d}` : d))((w === S.ws ? S.info : w.info)?.database);
      const lost = w === S.ws ? !$("lost").hidden : w.lost;
      const close = el("span", { className: "close", textContent: "×", title: "Close tab" });
      close.onclick = (e) => (e.stopPropagation(), closeDbTab(w));
      const b = el("button", {
        className: `db-tab ${w.conn.tag}`, draggable: true,
        title: `${w.conn.name}${database ? ` · ${database}` : ""}${lost ? " (connection lost)" : ""}`,
      },
        el("span", { className: "dot" }),
        el("span", { className: "label", textContent: database ?? w.conn.name }),
        ...(database ? [el("span", { className: "conn", textContent: w.conn.name })] : []),
        ...(lost ? [el("span", { className: "lost-mark", textContent: "!", title: "Connection lost" })] : []),
        close,
      );
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(!home && w === S.ws));
      b.onclick = () => (home || w !== S.ws) && showWorkspace(w);
      b.onauxclick = (e) => e.button === 1 && closeDbTab(w); // middle-click closes
      b.ondragstart = (e) => ((draggedTab = w), e.dataTransfer?.setData("text/plain", w.conn.name));
      b.ondragend = () => (draggedTab = null);
      b.ondragover = (e) => draggedTab && draggedTab !== w && e.preventDefault();
      b.ondrop = (e) => {
        e.preventDefault();
        if (!draggedTab || draggedTab === w) return;
        const r = b.getBoundingClientRect();
        S.workspaces.splice(S.workspaces.indexOf(draggedTab), 1);
        S.workspaces.splice(S.workspaces.indexOf(w) + (e.clientX > r.left + r.width / 2 ? 1 : 0), 0, draggedTab);
        draggedTab = null;
        renderDbTabs();
      };
      return b;
    }),
    ...(home ? [launcherTab()] : []),
    add,
    titleActions,
  );
  strip.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/** Closes a workspace tab; closing one in the background comes back to the tab on screen (or the launcher). */
async function closeDbTab(w: Workspace) {
  const home = !$("home").hidden;
  if (w === S.ws && !home) return closeWorkspace();
  const back = home ? null : S.ws;
  showWorkspace(w); // its tabs may have unsaved edits to ask about
  await closeWorkspace(back);
  if (home && S.workspaces.length) newWorkspace();
}
