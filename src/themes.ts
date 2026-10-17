// Themes: applying one (CSS variables on the root, plus its colour scheme), the light and dark defaults that Auto
// switches between, the theme editor, and importing and exporting theme files. The data side is theme-file.ts.
// Themes are kept with the settings (prefs.ts). Installing and updating them from a Git repository is TODO §23.

import { invoke } from "./ipc";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { $, el } from "./dom";
import { modal } from "./backup";
import { BUNDLED, MAX_THEME_FILE, VARS, gitSource, gitThemes, parseTheme, validValue, type GitSource, type Scheme, type Theme } from "./theme-file";
import { readPref, writePref } from "./prefs";

type Mode = "auto" | Scheme;
type Store = { light: string; dark: string; custom: Theme[]; git: GitSource[] };

const store: Store = (() => {
  const defaults: Store = { light: "TableMinus Light", dark: "TableMinus Dark", custom: [], git: [] };
  try {
    const s = { ...defaults, ...readPref<Partial<Store>>("themes") };
    // Each kept theme is checked again, so a hand-edited entry can't slip anything into the page.
    s.custom = (Array.isArray(s.custom) ? s.custom : []).flatMap((t: unknown) => {
      try {
        return [parseTheme(JSON.stringify(t))];
      } catch {
        return [];
      }
    });
    s.git = (Array.isArray(s.git) ? s.git : []).flatMap((g: unknown) => gitSource(g) ?? []);
    return s;
  } catch {
    return defaults;
  }
})();
function persist() {
  writePref("themes", store);
}

const all = () => [...BUNDLED, ...store.custom];
const byName = (name: string) => all().find((t) => t.name === name);
const isBundled = (t: Theme) => BUNDLED.includes(t);
function uniqueName(base: string) {
  let name = base.slice(0, 56);
  for (let n = 2; byName(name); n++) name = `${base.slice(0, 52)} ${n}`;
  return name;
}

// ---------- Applying ----------

const systemDark = matchMedia("(prefers-color-scheme: dark)");
let mode: Mode = "auto";
let previewing: Theme | null = null; // the theme open in the editor shows while it's edited

function shown(): Theme {
  if (previewing) return previewing;
  const scheme: Scheme = mode === "auto" ? (systemDark.matches ? "dark" : "light") : mode;
  return byName(store[scheme]) ?? BUNDLED[scheme === "dark" ? 1 : 0];
}

function apply() {
  const t = shown();
  const root = document.documentElement;
  for (const [v] of VARS) root.style.removeProperty(v);
  for (const [k, v] of Object.entries(t.vars)) root.style.setProperty(k, v);
  root.style.colorScheme = t.scheme;
}

/** Shows the theme for `m` (Settings › Appearance › Theme); Auto follows the system between the two defaults. */
export function applyTheme(m: Mode) {
  mode = m;
  apply();
}
systemDark.addEventListener("change", apply);

// ---------- Settings › Appearance ----------

const pick = (scheme: Scheme) => {
  const s = el("select");
  s.onchange = () => ((store[scheme] = s.value), persist(), apply());
  return s;
};
const lightPick = pick("light");
const darkPick = pick("dark");
const note = el("p", { className: "status" });
const fileInput = el("input", { type: "file", accept: ".json,application/json", hidden: true });

function fillPicks() {
  for (const [s, scheme] of [[lightPick, "light"], [darkPick, "dark"]] as const) {
    s.replaceChildren(...all().filter((t) => t.scheme === scheme).map((t) => el("option", { value: t.name, textContent: t.name })));
    s.value = byName(store[scheme])?.name ?? BUNDLED[scheme === "dark" ? 1 : 0].name;
  }
}

const row = (label: string, ...kids: Node[]) => el("div", { className: "row" }, el("span", { className: "k", textContent: label }), ...kids);
const button = (text: string, onclick: () => void) => el("button", { type: "button", textContent: text, onclick });
$("theme").closest(".row")!.after(
  row("Light theme", lightPick),
  row("Dark theme", darkPick),
  row("", el("span", { className: "row-items" }, button("Edit themes…", () => openEditor(shown())), button("Import…", () => fileInput.click()),
    button("Install from Git…", () => openGit())), fileInput),
  el("div", { className: "note" }, note),
);
fillPicks();

const say = (text: string, kind = "") => ((note.textContent = text), (note.className = `status ${kind}`));

fileInput.onchange = async () => {
  const file = fileInput.files?.[0];
  fileInput.value = "";
  if (!file) return;
  try {
    if (file.size > MAX_THEME_FILE) throw "The file is over 64 KB; that's no theme.";
    const t = parseTheme(await file.text());
    t.name = uniqueName(t.name);
    store.custom.push(t);
    persist();
    fillPicks();
    say(`Imported “${t.name}”. Choose it as the ${t.scheme} theme above.`, "done");
  } catch (e) {
    say(String(e), "error");
  }
};

// ---------- Themes from a Git repository (themes.rs) ----------

type Fetched = { commit: string; files: { path: string; text: string }[] };

/** Installs (or, for a repository already installed, replaces) the themes `fetched` from `url`. A theme picked
 *  before that the repository no longer has falls back to the default. */
function installGit(url: string, fetched: Fetched) {
  const { themes, skipped } = gitThemes(fetched.files);
  if (!themes.length) throw skipped.length ? `No themes in the repository. ${skipped.slice(0, 3).join("; ")}` : "No themes in the repository.";
  const before = store.git.find((g) => g.url === url);
  const old = new Set(before?.names ?? []);
  store.custom = store.custom.filter((t) => !old.has(t.name));
  const names: string[] = [];
  for (const t of themes) {
    t.name = uniqueName(t.name);
    store.custom.push(t);
    names.push(t.name);
  }
  const source: GitSource = { url, commit: fetched.commit, names };
  store.git = before ? store.git.map((g) => (g === before ? source : g)) : [...store.git, source];
  for (const sc of ["light", "dark"] as const) if (!byName(store[sc])) store[sc] = BUNDLED[sc === "dark" ? 1 : 0].name;
  persist();
  fillPicks();
  apply();
  return { names, skipped };
}

function removeGit(source: GitSource) {
  const names = new Set(source.names);
  store.custom = store.custom.filter((t) => !names.has(t.name));
  store.git = store.git.filter((g) => g !== source);
  for (const sc of ["light", "dark"] as const) if (!byName(store[sc])) store[sc] = BUNDLED[sc === "dark" ? 1 : 0].name;
  persist();
  fillPicks();
  apply();
}

function openGit() {
  const url = el("input", { type: "url", placeholder: "https://github.com/someone/tableminus-themes", spellcheck: false });
  const list = el("ul", { className: "git-themes" });
  const { d, footer, setStatus } = modal("Install themes from Git", "git-dialog", el("p", { className: "hint",
    textContent: "TableMinus clones the repository with git and installs every theme file (.json) at its top or in themes/. Updating replaces that repository's themes, including changes you made to them." }), url, list);
  const fetchFrom = async (address: string) => {
    setStatus("Fetching…", "busy");
    try {
      const got = await invoke<Fetched>("theme_git_fetch", { url: address });
      const { names, skipped } = installGit(address, got);
      setStatus(`Installed ${names.join(", ")}${skipped.length ? `. Skipped ${skipped.length}: ${skipped.slice(0, 3).join("; ")}` : ""}`, "done");
      render();
    } catch (e) {
      setStatus(String(e), "error");
    }
  };
  function render() {
    list.replaceChildren(...store.git.map((g) => el("li", {},
      el("span", { className: "grow" }, el("code", { textContent: g.url }), el("span", { className: "sub", textContent: `${g.names.join(", ")} · ${g.commit.slice(0, 7)}` })),
      button("Update", () => void fetchFrom(g.url)),
      button("Remove", () => (removeGit(g), render(), setStatus(`Removed the themes from ${g.url}`))))));
  }
  const install = el("button", { className: "primary", textContent: "Install" });
  install.onclick = () => url.value.trim() && void fetchFrom(url.value.trim());
  url.onkeydown = (e) => e.key === "Enter" && install.click();
  footer.append(el("span", { className: "spacer" }), button("Close", () => d.close()), install);
  render();
  d.showModal();
  url.focus();
}

async function exportTheme(t: Theme) {
  const path = await saveDialog({ defaultPath: `${t.name}.json`, filters: [{ name: "Theme", extensions: ["json"] }] });
  if (!path) return;
  await invoke("write_text_file", { path, text: JSON.stringify(t, null, 2) + "\n" });
}

// ---------- Theme editor ----------

// A colour as #rrggbb for the colour picker, whatever form the stylesheet gives it in.
const probe = el("span", { hidden: true });
document.body.append(probe);
const ctx = document.createElement("canvas").getContext("2d");
function hexOf(variable: string) {
  probe.style.color = `var(${variable})`;
  if (!ctx) return "#808080";
  ctx.fillStyle = "#808080";
  ctx.fillStyle = getComputedStyle(probe).color;
  return /^#[0-9a-f]{6}$/i.test(ctx.fillStyle) ? ctx.fillStyle : "#808080";
}

function openEditor(start: Theme) {
  let t = start;
  const choose = el("select");
  const body = el("div", { className: "theme-body" });
  const { d, footer, setStatus } = modal("Themes", "theme-editor", el("div", { className: "row-items" }, choose,
    button("Duplicate", () => {
      const copy: Theme = { name: uniqueName(`${t.name} copy`), scheme: t.scheme, vars: { ...t.vars } };
      store.custom.push(copy);
      changed();
      show(copy);
    }),
    button("Export…", () => exportTheme(t).catch((e) => setStatus(String(e), "error"))),
    button("Delete", () => {
      if (isBundled(t)) return;
      store.custom = store.custom.filter((x) => x !== t);
      for (const s of ["light", "dark"] as const) if (store[s] === t.name) store[s] = BUNDLED[s === "dark" ? 1 : 0].name;
      changed();
      show(BUNDLED[0]);
    })), body);
  footer.append(el("span", { className: "spacer" }), button("Done", () => d.close()));
  choose.onchange = () => show(byName(choose.value) ?? BUNDLED[0]);
  d.addEventListener("close", () => ((previewing = null), apply(), fillPicks()));

  function changed() {
    persist();
    previewing = t;
    apply();
  }

  function show(next: Theme) {
    t = next;
    previewing = t;
    apply();
    setStatus("");
    choose.replaceChildren(...all().map((x) => el("option", { value: x.name, textContent: `${x.name}${isBundled(x) ? " (bundled)" : ""}` })));
    choose.value = t.name;
    const locked = isBundled(t);
    const name = el("input", { value: t.name, disabled: locked, maxLength: 60 });
    name.onchange = () => {
      const v = name.value.trim();
      if (!v || (v !== t.name && byName(v))) return setStatus("Choose a name no other theme has.", "error"), (name.value = t.name);
      for (const s of ["light", "dark"] as const) if (store[s] === t.name) store[s] = v;
      t.name = v;
      changed();
      show(t);
    };
    const scheme = el("select", { disabled: locked }, el("option", { value: "light", textContent: "Light" }), el("option", { value: "dark", textContent: "Dark" }));
    scheme.value = t.scheme;
    scheme.onchange = () => ((t.scheme = scheme.value as Scheme), changed(), show(t));
    const groups = new Map<string, HTMLFieldSetElement>();
    for (const [variable, label, group, kind] of VARS) {
      const set = groups.get(group) ?? groups.set(group, el("fieldset", {}, el("legend", { textContent: group }))).get(group)!;
      set.append(el("label", {}, label, control(variable, kind, locked)));
    }
    body.replaceChildren(
      ...(locked ? [el("p", { className: "hint", textContent: "Bundled themes can't be changed. Duplicate one to make your own." })] : []),
      el("fieldset", {}, el("label", {}, "Name", name), el("label", {}, "Scheme", scheme)),
      ...groups.values(),
    );
  }

  /** The input for one variable; clearing it goes back to the default. */
  function control(variable: string, kind: "color" | "font" | "px", locked: boolean) {
    const value = t.vars[variable] ?? "";
    const set = (v: string) => {
      if (v === "") delete t.vars[variable];
      else if (validValue(kind, v)) t.vars[variable] = v;
      else return setStatus(`Not a valid value: ${v}`, "error"), false;
      setStatus("");
      changed();
      return true;
    };
    if (kind === "px") {
      const n = el("input", { type: "number", min: "0", max: "40", step: "1", className: "num-input", value: value.replace("px", ""), placeholder: "default", disabled: locked });
      n.onchange = () => set(n.value === "" ? "" : `${Number(n.value)}px`);
      return n;
    }
    const text = el("input", { value, placeholder: kind === "font" ? "default, e.g. Menlo, monospace" : "default", disabled: locked });
    if (kind === "font") {
      text.onchange = () => set(text.value.trim()) || (text.value = t.vars[variable] ?? "");
      return text;
    }
    const swatch = el("input", { type: "color", value: value.length === 7 ? value : hexOf(variable), disabled: locked });
    swatch.oninput = () => set(swatch.value) && (text.value = swatch.value);
    text.onchange = () => {
      if (!set(text.value.trim())) return void (text.value = t.vars[variable] ?? ""); // back to what's kept
      swatch.value = text.value.trim().length === 7 ? text.value.trim() : hexOf(variable);
    };
    return el("span", { className: "pick" }, swatch, text);
  }

  show(start);
  d.showModal();
}

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
dialog.theme-editor { width: min(620px, calc(100vw - 32px)); }
dialog.theme-editor[open] { display: grid; gap: 10px; }
dialog.theme-editor h3, dialog.theme-editor footer { margin: 0; }
.theme-body { max-height: min(60vh, 520px); overflow-y: auto; }
.theme-body legend { color: var(--muted); font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; padding: 0 4px; }
.theme-body label { grid-template-columns: 160px 1fr; }
.theme-body input[type="color"] { width: 40px; padding: 2px; flex: none; }
`);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
