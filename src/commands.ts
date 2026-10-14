// App commands: one list feeds the keyboard handler, the native menu bar, Open Anything and Settings › Shortcuts.
// Keys are matched in the capture phase, before the SQL editor or a focused field sees them, so a command's
// shortcut works wherever focus is; a command that isn't available right now lets the key through.
import { fromEvent, resolve } from "./keys";

export type MenuName = "File" | "Edit" | "View" | "Navigate" | "Connection" | "Tools" | "Window" | "Help";

export type Command = {
  id: string;
  label: string;
  menu: MenuName;
  /** Default shortcuts, "Mod" for ⌘ on macOS and Ctrl elsewhere. */
  keys?: string[];
  run: () => unknown;
  /** Whether the command applies right now (e.g. a table tab is open); otherwise its keys pass through. */
  when?: () => boolean;
  /** Draw a separator above it in the menu. */
  sep?: boolean;
  /** Runs while a dialog is open too (Settings). Everything else waits for the dialog to close. */
  overDialogs?: boolean;
  /** Left out of Open Anything (e.g. Tab 1…9, which only make sense as keys). */
  hidden?: boolean;
};

export const isMac = navigator.userAgent.includes("Mac");
export const commands: Command[] = [];
let overrides: Record<string, string[]> = {};
const changed: (() => void)[] = [];

export function addCommands(list: Command[]) {
  commands.push(...list);
}

/** The shortcuts in use for `id`: the user's own, or the defaults. */
export function bindings(id: string): string[] {
  const c = commands.find((c) => c.id === id);
  const keys = overrides[id] ?? c?.keys ?? [];
  return keys.map((k) => resolve(k, isMac)).filter(Boolean);
}

/** Replaces the user's shortcuts (id → keys; a missing id keeps its defaults). */
export function setOverrides(o: Record<string, string[]>) {
  if (JSON.stringify(o) === JSON.stringify(overrides)) return; // every settings change passes through here
  overrides = o;
  for (const f of changed) f();
}

export const onBindingsChanged = (f: () => void) => changed.push(f);

const dialogOpen = () => !!document.querySelector("dialog[open]");

/** Runs a command if it applies right now; true when it ran. */
export function runCommand(id: string): boolean {
  const c = commands.find((c) => c.id === id);
  if (!c || (!c.overDialogs && dialogOpen()) || (c.when && !c.when())) return false;
  c.run();
  return true;
}

/** Set while Settings › Shortcuts records a key, so the press is captured instead of run. */
export let recording: ((binding: string) => void) | null = null;
export const setRecording = (f: typeof recording) => (recording = f);

window.addEventListener(
  "keydown",
  (e) => {
    const pressed = fromEvent(e);
    if (!pressed) return;
    if (recording) {
      e.preventDefault();
      e.stopPropagation();
      if (pressed === "Escape") recording("");
      else if (pressed.includes("+")) recording(pressed); // a bare key would shadow typing
      return;
    }
    for (const c of commands) {
      if (bindings(c.id).includes(pressed) && runCommand(c.id)) {
        e.preventDefault();
        e.stopPropagation();
        return;
      }
    }
  },
  true,
);
