// The settings, their defaults, and reading stored ones back over the defaults. settings.ts keeps them and wires the
// Settings window.

import type { CompleteKey, FormatStyle } from "../editor/editor";
import type { ParamFormat } from "../editor/statements";
import type { DefaultColumn, Op } from "../workspace/filters";
import type { Safety } from "./safety";

export type Settings = {
  theme: "auto" | "light" | "dark"; zebra: boolean; fontSize: number; lineHeight: number; indent: "tab" | "2" | "4";
  /** Safe mode level for new connections. */
  safeLevel: Safety; queryTimeout: number; keepAlive: boolean;
  /** Most rows an editor query shows; 0 = no limit. */
  sqlLimit: number;
  formatStyle: FormatStyle;
  paramFormat: ParamFormat | "off";
  /** The SQL editor's keys: CodeMirror's Sublime-like defaults, or Vim. */
  keyBinding: "sublime" | "vim";
  /** Completions write keywords in upper case. */
  upperKeywords: boolean;
  /** The key that accepts a completion. */
  completeKey: CompleteKey;
  /** A new filter row's column and operator, and whether a table opens with the filter bar showing. */
  filterColumn: DefaultColumn; filterOp: Op; filterBar: boolean;
  wrapLines: boolean;
  showInvisibles: boolean;
  /** Save each connection's SQL tabs and reopen them on connect. */
  keepTabs: boolean;
  /** Several result sets: a tab each, or all appended in one. */
  resultsMode: "tabs" | "append";
  /** Running several statements stops at the first that fails. */
  stopOnError: boolean;
  /** How many years of query history to keep; 0 = forever. */
  historyYears: number;
  /** Rebound shortcuts, command id → keys (see commands.ts); a missing id keeps its defaults. */
  shortcuts: Record<string, string[]>;
  launcherSort: "name" | "kind" | "tag";
  /** Reconnect, at startup, the workspaces open at quit. */
  reopenWorkspaces: boolean;
  /** What ⌘T opens: a SQL tab, or an empty one that the next table or query fills. */
  newTabKind: "sql" | "empty";
  /** Look for a new version at startup; and take beta versions. */
  autoUpdate: boolean; betaUpdates: boolean;
  /** Rows on a table tab's page. */
  pageSize: number;
  /** Tables the catalog estimates at this many rows or more show the estimate instead of a count; 0 = always count. */
  estimateOver: number;
  /** The sort a table tab opens with. */
  defaultSort: "none" | "asc" | "desc";
  /** PostGIS geometry and geography as WKT rather than hex WKB. */
  geometryWkt: boolean;
  /** Overlay scroll bars on the grid, shown while scrolling. */
  hideScrollbars: boolean;
  /** Write the values updated and deleted rows had to the console log when saving. */
  logOriginals: boolean;
  /** Show an export's files in Finder once it finishes. */
  revealExport: boolean;
  closeBrackets: boolean;
};
export const DEFAULT_SETTINGS: Settings = {
  theme: "auto", zebra: true, fontSize: 13, lineHeight: 1.5, indent: "2", safeLevel: "alert2", queryTimeout: 300, keepAlive: true,
  sqlLimit: 1000, formatStyle: "standard", paramFormat: "colon", wrapLines: false, showInvisibles: false, keepTabs: true,
  keyBinding: "sublime", upperKeywords: false, completeKey: "enter", filterColumn: "any", filterOp: "contains", filterBar: false,
  shortcuts: {}, launcherSort: "name", reopenWorkspaces: false, newTabKind: "sql", autoUpdate: true, betaUpdates: false,
  resultsMode: "tabs", stopOnError: true, historyYears: 0,
  pageSize: 300, estimateOver: 1_000_000, defaultSort: "none", geometryWkt: true, hideScrollbars: false, logOriginals: false,
  revealExport: false, closeBrackets: true,
};

/** Stored settings over the defaults. */
export function fromStored(stored: Record<string, unknown> | undefined): Settings {
  const s = { ...stored };
  if (typeof s.safeDefault === "boolean" && !s.safeLevel) s.safeLevel = s.safeDefault ? "alert2" : "silent"; // before levels
  delete s.safeDefault;
  return { ...DEFAULT_SETTINGS, ...s };
}
