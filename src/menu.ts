// The native menu bar, laid out like TablePlus's: File, Edit, View, Navigate, Connection, Tools, Window, Help.
// Every item is a command from commands.ts, so the menu, the keyboard and Open Anything run the same code, and
// rebinding a shortcut rebuilds the menu with the new accelerator. Only the app window has a menu bar; in a plain
// browser preview this does nothing.
import { Menu, type MenuItemOptions, type PredefinedMenuItemOptions, type SubmenuOptions } from "@tauri-apps/api/menu";
import { bindings, commands, isMac, onBindingsChanged, runCommand, type MenuName } from "./commands";

type Entry = MenuItemOptions | SubmenuOptions | PredefinedMenuItemOptions;

const ORDER: MenuName[] = ["File", "Edit", "View", "Navigate", "Connection", "Tools", "Window", "Help"];
const sep: PredefinedMenuItemOptions = { item: "Separator" };

/** The menu entries for one menu's commands, with separators where a command asks for one. */
function entries(menu: MenuName): Entry[] {
  const out: Entry[] = [];
  const tabs: Entry[] = [];
  for (const c of commands.filter((c) => c.menu === menu)) {
    const item: MenuItemOptions = { id: c.id, text: c.label, accelerator: bindings(c.id)[0], action: () => void runCommand(c.id) };
    if (c.id.startsWith("tab-")) {
      tabs.push(item); // ⌘1…⌘9 go together in a submenu, as in TablePlus
      if (tabs.length === 1) out.push(...(c.sep && out.length ? [sep] : []), { text: "Select Tab", items: tabs });
      continue;
    }
    if (c.sep && out.length) out.push(sep);
    out.push(item);
  }
  return out;
}

async function build() {
  const edit: Entry[] = [
    { item: "Undo" }, { item: "Redo" }, sep, { item: "Cut" }, { item: "Copy" }, { item: "Paste" }, { item: "SelectAll" },
  ];
  const window: Entry[] = [{ item: "Minimize" }, { item: "Maximize" }, ...(isMac ? [{ item: "Fullscreen" } as Entry] : []), sep];
  const app: SubmenuOptions = {
    text: "DB Manager",
    items: [
      { item: { About: null } }, sep,
      ...commands.filter((c) => c.id === "settings").map((c) => ({ text: c.label, accelerator: bindings(c.id)[0], action: () => void runCommand(c.id) })),
      sep, { item: "Services" }, sep, { item: "Hide" }, { item: "HideOthers" }, { item: "ShowAll" }, sep, { item: "Quit" },
    ],
  };
  const menus: SubmenuOptions[] = ORDER.map((name) => {
    // On macOS Settings lives in the app menu; elsewhere it stays in File.
    let items = entries(name).filter((e) => !(isMac && "id" in e && e.id === "settings"));
    if (name === "Edit") items = [...edit, ...(items.length ? [sep] : []), ...items];
    if (name === "Window") items = [...window, ...items];
    return { text: name, items };
  });
  const menu = await Menu.new({ items: isMac ? [app, ...menus] : menus });
  await menu.setAsAppMenu();
}

let pending = Promise.resolve();
/** Builds the menu now and again whenever a shortcut changes. */
export function initMenu() {
  if (!("__TAURI_INTERNALS__" in window)) return;
  const rebuild = () => (pending = pending.then(build).catch((e) => console.warn("menu bar:", e)));
  rebuild();
  onBindingsChanged(rebuild);
}
