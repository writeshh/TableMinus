// Keyboard shortcuts as strings such as "Meta+Shift+KeyT": modifiers in a fixed order (Ctrl, Alt, Shift, Meta),
// then the physical key as KeyboardEvent.code. The same string is a Tauri menu accelerator, so the menu bar shows
// exactly what the keyboard handler matches. Defaults say "Mod" for ⌘ on macOS and Ctrl elsewhere. No DOM here,
// so the node tests can import it.

const MODS = ["Ctrl", "Alt", "Shift", "Meta"] as const;

/** A binding with "Mod" replaced for this platform and its modifiers put in order; "" when it has no key. */
export function resolve(binding: string, mac: boolean): string {
  const parts = binding.split("+").map((p) => (p === "Mod" ? (mac ? "Meta" : "Ctrl") : p));
  const key = parts.pop() ?? "";
  if (!key || (MODS as readonly string[]).includes(key)) return "";
  return [...MODS.filter((m) => parts.includes(m)), key].join("+");
}

type KeyLike = { ctrlKey: boolean; altKey: boolean; shiftKey: boolean; metaKey: boolean; code: string };

/** The binding a key press makes; "" for a bare modifier. */
export function fromEvent(e: KeyLike): string {
  if (!e.code || /^(Control|Alt|Shift|Meta|OS)(Left|Right)?$/.test(e.code)) return "";
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Meta"].filter(Boolean);
  return [...mods, e.code].join("+");
}

const NAMES: Record<string, string> = {
  BracketLeft: "[", BracketRight: "]", Comma: ",", Period: ".", Slash: "/", Backslash: "\\", Semicolon: ";",
  Quote: "'", Backquote: "`", Minus: "-", Equal: "=", Enter: "↩", Tab: "⇥", Space: "Space", Escape: "Esc",
  Backspace: "⌫", Delete: "⌦", ArrowLeft: "←", ArrowRight: "→", ArrowUp: "↑", ArrowDown: "↓",
};
const MAC_MODS: Record<string, string> = { Ctrl: "⌃", Alt: "⌥", Shift: "⇧", Meta: "⌘" };

/** How a binding reads on screen: "⌘⇧T" on macOS, "Ctrl+Shift+T" elsewhere. */
export function label(binding: string, mac: boolean): string {
  if (!binding) return "";
  const parts = binding.split("+");
  const code = parts.pop()!;
  const key = NAMES[code] ?? code.replace(/^(Key|Digit|Numpad)/, "");
  return mac ? parts.map((m) => MAC_MODS[m] ?? m).join("") + key : [...parts, key].join("+");
}

/** Every binding used by more than one command, with those commands' ids. */
export function conflicts(bindings: Record<string, string[]>): Map<string, string[]> {
  const users = new Map<string, string[]>();
  for (const [id, keys] of Object.entries(bindings)) for (const k of new Set(keys)) users.set(k, [...(users.get(k) ?? []), id]);
  return new Map([...users].filter(([, ids]) => ids.length > 1));
}
