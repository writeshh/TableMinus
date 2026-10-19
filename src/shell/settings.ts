// The settings (⌘,): kept with the other preferences in settings.json (prefs.ts), reflected into the page, the
// Settings window's controls and the backend. Other modules (themes, passcode, locations, mcp, ai, plugins) add tabs
// of their own.

import { invoke } from "../ipc";
import { $, el } from "../dom";
import type { Editor } from "../editor/editor";
import { current, status } from "../workspace/state";
import { DEFAULT_SETTINGS, fromStored, type Settings } from "./settings-defaults";
import { readPref, writePref } from "./prefs";
import { applyTheme } from "./themes";
import { showResults } from "../workspace/run";
import { OPS, setFilterDefaults, type DefaultColumn, type Op } from "../workspace/filters";
import { setOverrides } from "./commands";
import { renderShortcuts } from "./shortcuts";
import { csvDefaults } from "../database/importexport";
import { fillLevels, type Safety } from "./safety";
import type { CompleteKey, FormatStyle } from "../editor/editor";
import { renderConns } from "../connections/home";
import { renderHistory } from "../workspace/history";

/** The SQL editor, which follows the settings (main.ts). */
type Host = {
  editor: Editor;
};
let host: Host;

export let settings: Settings = fromStored(readPref<Record<string, unknown>>("settings"));

export function setSetting<K extends keyof Settings>(key: K, value: Settings[K]) {
  settings[key] = value;
  writePref("settings", settings);
  applySettings();
}

function applySettings() {
  const root = document.documentElement;
  applyTheme(settings.theme);
  root.style.setProperty("--editor-size", `${settings.fontSize}px`);
  root.style.setProperty("--editor-line", String(settings.lineHeight));
  document.body.classList.toggle("zebra", settings.zebra);
  // Reflect into the settings controls (they may be open while a shortcut changes a value).
  for (const b of $("theme").querySelectorAll("button")) b.setAttribute("aria-pressed", String(b.dataset.value === settings.theme));
  $<HTMLInputElement>("zebra").checked = settings.zebra;
  $<HTMLInputElement>("font-size").value = String(settings.fontSize);
  $<HTMLInputElement>("line-height").value = String(settings.lineHeight);
  $<HTMLSelectElement>("indent").value = settings.indent;
  host.editor.setIndent(settings.indent === "tab" ? "\t" : " ".repeat(Number(settings.indent)));
  $<HTMLSelectElement>("safe-default").value = settings.safeLevel;
  $<HTMLInputElement>("query-timeout").value = String(settings.queryTimeout);
  $<HTMLInputElement>("keep-alive").checked = settings.keepAlive;
  $<HTMLSelectElement>("row-limit").value = String(settings.sqlLimit);
  $<HTMLSelectElement>("format-style").value = settings.formatStyle;
  $<HTMLSelectElement>("param-format").value = settings.paramFormat;
  $<HTMLInputElement>("wrap-lines").checked = settings.wrapLines;
  $<HTMLInputElement>("show-invisibles").checked = settings.showInvisibles;
  $<HTMLInputElement>("keep-tabs").checked = settings.keepTabs;
  $<HTMLInputElement>("reopen-workspaces").checked = settings.reopenWorkspaces;
  $<HTMLSelectElement>("new-tab-kind").value = settings.newTabKind;
  $<HTMLInputElement>("auto-update").checked = settings.autoUpdate;
  $<HTMLInputElement>("beta-updates").checked = settings.betaUpdates;
  $<HTMLSelectElement>("conn-sort").value = settings.launcherSort;
  $<HTMLSelectElement>("results-mode").value = settings.resultsMode;
  $<HTMLInputElement>("stop-on-error").checked = settings.stopOnError;
  $<HTMLSelectElement>("history-years").value = String(settings.historyYears);
  $<HTMLInputElement>("page-size").value = String(settings.pageSize);
  $<HTMLInputElement>("estimate-over").value = String(settings.estimateOver);
  $<HTMLSelectElement>("default-sort").value = settings.defaultSort;
  $<HTMLInputElement>("geometry-wkt").checked = settings.geometryWkt;
  $<HTMLInputElement>("hide-scrollbars").checked = settings.hideScrollbars;
  $<HTMLInputElement>("log-originals").checked = settings.logOriginals;
  $<HTMLInputElement>("reveal-export").checked = settings.revealExport;
  $<HTMLInputElement>("close-brackets").checked = settings.closeBrackets;
  document.body.classList.toggle("hide-scrollbars", settings.hideScrollbars);
  const tab = current();
  if (tab?.kind === "sql" && !$("workspace").hidden) showResults(tab);
  host.editor.setDisplay(settings.wrapLines, settings.showInvisibles);
  $<HTMLSelectElement>("key-binding").value = settings.keyBinding;
  $<HTMLInputElement>("upper-keywords").checked = settings.upperKeywords;
  $<HTMLSelectElement>("complete-key").value = settings.completeKey;
  $<HTMLSelectElement>("filter-column").value = settings.filterColumn;
  $<HTMLSelectElement>("filter-op").value = settings.filterOp;
  $<HTMLInputElement>("filter-bar").checked = settings.filterBar;
  host.editor.setCompletion(settings.upperKeywords, settings.completeKey);
  host.editor.setCloseBrackets(settings.closeBrackets);
  host.editor.setKeyBinding(settings.keyBinding).catch((e) => status(`Vim keys: ${e}`, "error"));
  setFilterDefaults({ column: settings.filterColumn, op: settings.filterOp });
  setOverrides(settings.shortcuts);
  // The backend enforces these; outside the app (a plain browser preview) there's no backend to tell.
  invoke("set_preferences", { keepAlive: settings.keepAlive, queryTimeoutSecs: settings.queryTimeout, maxRows: settings.sqlLimit }).catch(() => {});
}

export function showTab(tab: string) {
  if (tab === "shortcuts") renderShortcuts($("shortcuts-pane"), settings.shortcuts, (o) => setSetting("shortcuts", o));
  for (const t of $("settings").querySelectorAll<HTMLElement>("[role=tab]")) t.setAttribute("aria-selected", String(t.dataset.tab === tab));
  for (const p of $("settings").querySelectorAll<HTMLElement>("[data-pane]")) p.hidden = p.dataset.pane !== tab;
}

export const MAX_PAGE = 100_000; // sql/browse.rs MAX_PAGE

export const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));
export const openSettings = () => {
  const d = $<HTMLDialogElement>("settings");
  csvDefaults($("csv-defaults")); // the export dialog may have changed them since
  if (!d.open) d.showModal();
};

export function initSettings(h: Host) {
  host = h;
  for (const b of document.querySelectorAll<HTMLElement>(".open-settings")) b.onclick = openSettings;
  for (const t of $("settings").querySelectorAll<HTMLElement>("[role=tab]")) t.onclick = () => showTab(t.dataset.tab!);
  for (const b of $("theme").querySelectorAll<HTMLButtonElement>("button")) b.onclick = () => setSetting("theme", b.dataset.value as Settings["theme"]);
  $<HTMLInputElement>("zebra").onchange = (e) => setSetting("zebra", (e.target as HTMLInputElement).checked);
  $<HTMLInputElement>("font-size").onchange = (e) => setSetting("fontSize", clamp(Number((e.target as HTMLInputElement).value) || 13, 9, 28));
  $<HTMLInputElement>("line-height").onchange = (e) => setSetting("lineHeight", clamp(Number((e.target as HTMLInputElement).value) || 1.5, 1.1, 2.2));
  $<HTMLSelectElement>("indent").onchange = (e) => setSetting("indent", (e.target as HTMLSelectElement).value as Settings["indent"]);
  $<HTMLInputElement>("query-timeout").onchange = (e) => setSetting("queryTimeout", clamp(Math.round(Number((e.target as HTMLInputElement).value) || 0), 0, 86400));
  $<HTMLInputElement>("keep-alive").onchange = (e) => setSetting("keepAlive", (e.target as HTMLInputElement).checked);
  fillLevels($("safe-default"));
  $<HTMLSelectElement>("safe-default").onchange = (e) => setSetting("safeLevel", (e.target as HTMLSelectElement).value as Safety);
  $<HTMLSelectElement>("key-binding").onchange = (e) => setSetting("keyBinding", (e.target as HTMLSelectElement).value as Settings["keyBinding"]);
  $<HTMLInputElement>("upper-keywords").onchange = (e) => setSetting("upperKeywords", (e.target as HTMLInputElement).checked);
  $<HTMLSelectElement>("complete-key").onchange = (e) => setSetting("completeKey", (e.target as HTMLSelectElement).value as CompleteKey);
  $("filter-op").replaceChildren(...OPS.filter(([op]) => op !== "raw").map(([op, label]) => el("option", { value: op, textContent: label })));
  $<HTMLSelectElement>("filter-column").onchange = (e) => setSetting("filterColumn", (e.target as HTMLSelectElement).value as DefaultColumn);
  $<HTMLSelectElement>("filter-op").onchange = (e) => setSetting("filterOp", (e.target as HTMLSelectElement).value as Op);
  $<HTMLInputElement>("filter-bar").onchange = (e) => setSetting("filterBar", (e.target as HTMLInputElement).checked);
  $("settings-reset").onclick = () => {
    settings = { ...DEFAULT_SETTINGS };
    setSetting("theme", settings.theme); // persists and re-applies everything
    if (!$("shortcuts-pane").hidden) showTab("shortcuts");
  };
  $("settings-done").onclick = () => $<HTMLDialogElement>("settings").close();

  showTab("general");
  applySettings();
  $<HTMLInputElement>("wrap-lines").onchange = (e) => setSetting("wrapLines", (e.target as HTMLInputElement).checked);
  $<HTMLInputElement>("show-invisibles").onchange = (e) => setSetting("showInvisibles", (e.target as HTMLInputElement).checked);
  $<HTMLInputElement>("keep-tabs").onchange = (e) => setSetting("keepTabs", (e.target as HTMLInputElement).checked);
  $<HTMLInputElement>("reopen-workspaces").onchange = (e) => setSetting("reopenWorkspaces", (e.target as HTMLInputElement).checked);
  $<HTMLSelectElement>("new-tab-kind").onchange = (e) => setSetting("newTabKind", (e.target as HTMLSelectElement).value as Settings["newTabKind"]);
  $<HTMLInputElement>("auto-update").onchange = (e) => setSetting("autoUpdate", (e.target as HTMLInputElement).checked);
  $<HTMLInputElement>("beta-updates").onchange = (e) => setSetting("betaUpdates", (e.target as HTMLInputElement).checked);
  $<HTMLSelectElement>("conn-sort").onchange = (e) => (setSetting("launcherSort", (e.target as HTMLSelectElement).value as Settings["launcherSort"]), renderConns());
  $<HTMLSelectElement>("results-mode").onchange = (e) => setSetting("resultsMode", (e.target as HTMLSelectElement).value as Settings["resultsMode"]);
  $<HTMLInputElement>("stop-on-error").onchange = (e) => setSetting("stopOnError", (e.target as HTMLInputElement).checked);
  $<HTMLSelectElement>("history-years").onchange = (e) => (setSetting("historyYears", Number((e.target as HTMLSelectElement).value)), renderHistory());
  $<HTMLSelectElement>("param-format").onchange = (e) => setSetting("paramFormat", (e.target as HTMLSelectElement).value as Settings["paramFormat"]);
  $<HTMLSelectElement>("format-style").onchange = (e) => setSetting("formatStyle", (e.target as HTMLSelectElement).value as FormatStyle);
  $<HTMLSelectElement>("row-limit").onchange = (e) => setSetting("sqlLimit", Number((e.target as HTMLSelectElement).value));
  $<HTMLInputElement>("page-size").onchange = (e) => setSetting("pageSize", clamp(Math.round(Number((e.target as HTMLInputElement).value)) || 300, 1, MAX_PAGE));
  $<HTMLInputElement>("estimate-over").onchange = (e) => setSetting("estimateOver", Math.max(0, Math.round(Number((e.target as HTMLInputElement).value)) || 0));
  $<HTMLSelectElement>("default-sort").onchange = (e) => setSetting("defaultSort", (e.target as HTMLSelectElement).value as Settings["defaultSort"]);
  $<HTMLInputElement>("geometry-wkt").onchange = (e) => setSetting("geometryWkt", (e.target as HTMLInputElement).checked);
  $<HTMLInputElement>("hide-scrollbars").onchange = (e) => setSetting("hideScrollbars", (e.target as HTMLInputElement).checked);
  $<HTMLInputElement>("log-originals").onchange = (e) => setSetting("logOriginals", (e.target as HTMLInputElement).checked);
  $<HTMLInputElement>("reveal-export").onchange = (e) => setSetting("revealExport", (e.target as HTMLInputElement).checked);
  $<HTMLInputElement>("close-brackets").onchange = (e) => setSetting("closeBrackets", (e.target as HTMLInputElement).checked);
}
