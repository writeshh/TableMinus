// Themes as data: what a theme may set, the bundled ones, and reading a theme file someone shares (validated, since
// it comes from outside). The DOM side is themes.ts.

export type Scheme = "light" | "dark";
/** `vars` holds only what the theme changes; anything else keeps styles.css's default. */
export type Theme = { name: string; scheme: Scheme; vars: Record<string, string> };
type Kind = "color" | "font" | "px";

/** Every CSS variable a theme may set: [variable, label, group, kind]. */
export const VARS: [string, string, string, Kind][] = [
  ["--bg", "Background", "Window", "color"],
  ["--fg", "Text", "Window", "color"],
  ["--muted", "Secondary text", "Window", "color"],
  ["--accent", "Accent", "Window", "color"],
  ["--line", "Lines", "Window", "color"],
  ["--hover", "Hover", "Window", "color"],
  ["--raised", "Panels and menus", "Window", "color"],
  ["--side", "Background", "Sidebar", "color"],
  ["--side-fg", "Text", "Sidebar", "color"],
  ["--tabs-bg", "Tab bar", "Tabs", "color"],
  ["--tab-active", "Selected tab", "Tabs", "color"],
  ["--grid-bg", "Background", "Grid", "color"],
  ["--grid-head", "Header", "Grid", "color"],
  ["--grid-line", "Lines", "Grid", "color"],
  ["--grid-zebra", "Alternate rows", "Grid", "color"],
  ["--grid-sel", "Selection", "Grid", "color"],
  ["--grid-null", "NULL", "Grid", "color"],
  ["--grid-font", "Font", "Grid", "font"],
  ["--grid-size", "Font size", "Grid", "px"],
  ["--grid-pad-y", "Cell padding, vertical", "Grid", "px"],
  ["--grid-pad-x", "Cell padding, horizontal", "Grid", "px"],
  ["--editor-bg", "Background", "Editor", "color"],
  ["--editor-font", "Font", "Editor", "font"],
  ["--editor-pad", "Padding", "Editor", "px"],
  ["--syn-keyword", "Keywords", "Editor", "color"],
  ["--syn-type", "Types", "Editor", "color"],
  ["--syn-function", "Functions", "Editor", "color"],
  ["--syn-string", "Strings", "Editor", "color"],
  ["--syn-quoted", "Quoted names", "Editor", "color"],
  ["--syn-number", "Numbers", "Editor", "color"],
  ["--syn-comment", "Comments", "Editor", "color"],
  ["--ok", "Success", "Status", "color"],
  ["--warn", "Warning", "Status", "color"],
  ["--danger", "Error and danger", "Status", "color"],
];

/** The largest theme file accepted. */
export const MAX_THEME_FILE = 64 * 1024;

const COLOR = /^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const FONT = /^[\p{L}\p{N} "',._-]{1,120}$/u;
const PX = /^(\d{1,2})(\.\d)?px$/;

/** Whether `value` is allowed for a variable of `kind`: hex colours, plain font lists, 0 to 40 px. */
export function validValue(kind: Kind, value: string) {
  if (kind === "color") return COLOR.test(value);
  if (kind === "font") return FONT.test(value);
  const m = PX.exec(value);
  return m !== null && Number(m[1]) <= 40;
}

/** A theme from a file's text; throws a message saying what's wrong. */
export function parseTheme(text: string): Theme {
  if (text.length > MAX_THEME_FILE) throw "The file is over 64 KB; that's no theme.";
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    throw "The file isn't JSON.";
  }
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) throw "A theme is a JSON object.";
  const t = raw as Record<string, unknown>;
  const extra = Object.keys(t).filter((k) => !["name", "scheme", "vars"].includes(k));
  if (extra.length) throw `Unknown field: ${extra[0]}`;
  if (typeof t.name !== "string" || !t.name.trim() || t.name.length > 60) throw "The theme needs a name of up to 60 characters.";
  if (t.scheme !== "light" && t.scheme !== "dark") throw `"scheme" must be "light" or "dark".`;
  if (typeof t.vars !== "object" || t.vars === null || Array.isArray(t.vars)) throw `"vars" must be an object.`;
  const vars: Record<string, string> = {};
  for (const [key, value] of Object.entries(t.vars)) {
    const known = VARS.find(([v]) => v === key);
    if (!known) throw `Unknown variable: ${key}`;
    if (typeof value !== "string" || !validValue(known[3], value)) throw `Not a valid value for ${key}: ${JSON.stringify(value)}`;
    vars[key] = value;
  }
  return { name: t.name.trim(), scheme: t.scheme, vars };
}

/** The brand look for each scheme (styles.css's defaults), then our other palettes. */
export const BUNDLED: Theme[] = [
  { name: "TableMinus Light", scheme: "light", vars: {} },
  { name: "TableMinus Dark", scheme: "dark", vars: {} },
  {
    name: "Paper", scheme: "light", vars: {
      "--bg": "#fbf8f1", "--fg": "#2b2620", "--muted": "#7d7366", "--accent": "#b4561f", "--line": "#e6dfd2",
      "--hover": "#f1ebdf", "--raised": "#f6f1e7", "--side": "#f3ede1", "--grid-head": "#f6f1e7", "--grid-zebra": "#f7f3ea",
      "--syn-keyword": "#9c3d10", "--syn-type": "#356b5a", "--syn-function": "#7a5a00", "--syn-string": "#4f7a28",
      "--syn-quoted": "#8a3f6d", "--syn-number": "#2f5f9e", "--syn-comment": "#9a8f80",
    },
  },
  {
    name: "Harbor", scheme: "light", vars: {
      "--bg": "#f6f8fb", "--fg": "#1d2733", "--muted": "#66768a", "--accent": "#1f6fd1", "--line": "#dde3eb",
      "--hover": "#e9eef5", "--raised": "#eef2f7", "--side": "#e8edf4", "--tabs-bg": "#eef2f7", "--grid-head": "#eef2f7",
      "--syn-keyword": "#1f5fbf", "--syn-type": "#0e7c86", "--syn-function": "#7646b8", "--syn-string": "#b04a2e",
      "--syn-quoted": "#a33f7a", "--syn-number": "#0b7a3e", "--syn-comment": "#7d8a99",
    },
  },
  {
    name: "Ember", scheme: "dark", vars: {
      "--bg": "#1e1b19", "--fg": "#ece4dc", "--muted": "#a39789", "--accent": "#f08a3c", "--line": "#3a332e",
      "--hover": "#2c2724", "--raised": "#262220", "--side": "#191715", "--grid-head": "#262220", "--grid-zebra": "#221f1c",
      "--syn-keyword": "#f2a65a", "--syn-type": "#8fc7a7", "--syn-function": "#e6c36a", "--syn-string": "#c4d47c",
      "--syn-quoted": "#e48aa8", "--syn-number": "#9ab8f0", "--syn-comment": "#7f746a",
    },
  },
  {
    name: "Lagoon", scheme: "dark", vars: {
      "--bg": "#0f1d22", "--fg": "#d7ebee", "--muted": "#86a3aa", "--accent": "#2fbfb1", "--line": "#20343b",
      "--hover": "#17292f", "--raised": "#132429", "--side": "#0c181c", "--tabs-bg": "#0c181c", "--grid-head": "#132429",
      "--syn-keyword": "#58c4e0", "--syn-type": "#7ee0b5", "--syn-function": "#c8a6f2", "--syn-string": "#f0b27a",
      "--syn-quoted": "#f28fb0", "--syn-number": "#e6d36e", "--syn-comment": "#5e7c84",
    },
  },
];
