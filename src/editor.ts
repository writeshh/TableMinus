import { EditorView, basicSetup } from "codemirror";
import { Compartment, EditorSelection, EditorState, Prec, RangeSetBuilder, type Extension } from "@codemirror/state";
import { Decoration, ViewPlugin, highlightWhitespace, keymap, placeholder, type DecorationSet, type ViewUpdate } from "@codemirror/view";
import { indentLess, indentMore, indentWithTab } from "@codemirror/commands";
import { acceptCompletion, autocompletion, completionKeymap, type CompletionContext } from "@codemirror/autocomplete";
import { HighlightStyle, indentUnit, syntaxHighlighting } from "@codemirror/language";
import { MSSQL, MariaSQL, MySQL, PostgreSQL, SQLite, StandardSQL, sql } from "@codemirror/lang-sql";
import { highlightCode, tags as t } from "@lezer/highlight";
import { sqlKind, statementAt, splitStatements, type Dialect, type Statement } from "./statements";
import { clickhouse, formatDialect, mariadb, mysql, postgresql, sqlite, transactsql } from "sql-formatter";
import { byLine, transformText, type Transform } from "./transforms";

// DuckDB's SQL follows PostgreSQL's, so it borrows its highlighting and formatting.
const FORMAT_DIALECTS = { postgres: postgresql, mysql, mariadb, sqlite, duckdb: postgresql, mssql: transactsql, clickhouse } as const;
export type FormatStyle = "standard" | "tabularLeft" | "tabularRight";

const DIALECTS = { postgres: PostgreSQL, mysql: MySQL, mariadb: MariaSQL, sqlite: SQLite, duckdb: PostgreSQL, mssql: MSSQL, clickhouse: StandardSQL } as const;

// Colours are CSS variables (see styles.css) so they follow the app's light/dark theme.
const highlight = HighlightStyle.define([
  { tag: t.keyword, color: "var(--syn-keyword)" },
  { tag: t.typeName, color: "var(--syn-type)" },
  { tag: t.standard(t.name), color: "var(--syn-function)" },
  { tag: [t.string, t.special(t.name)], color: "var(--syn-string)" },
  { tag: t.special(t.string), color: "var(--syn-quoted)" }, // "quoted" / `quoted` identifiers
  { tag: [t.number, t.bool, t.null], color: "var(--syn-number)" },
  { tag: [t.lineComment, t.blockComment], color: "var(--syn-comment)", fontStyle: "italic" },
  { tag: [t.operator, t.punctuation, t.paren], color: "var(--muted)" },
  { tag: [t.function(t.variableName), t.function(t.propertyName)], color: "var(--syn-function)" }, // scripts (scripts.ts)
]);

const theme = EditorView.theme({
  "&": { height: "100%", fontSize: "var(--editor-size, 13px)", color: "var(--fg)", backgroundColor: "transparent" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--editor-font)", lineHeight: "var(--editor-line, 1.5)" },
  ".cm-content": { caretColor: "var(--accent)", padding: "var(--editor-pad) 0" },
  ".cm-cursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "2px" },
  ".cm-gutters": { backgroundColor: "transparent", color: "var(--muted)", border: "none", paddingLeft: "4px" },
  ".cm-activeLine": { backgroundColor: "var(--hover)" },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--fg)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
    { backgroundColor: "color-mix(in srgb, var(--accent) 32%, transparent)" },
  ".cm-matchingBracket": { backgroundColor: "color-mix(in srgb, var(--accent) 22%, transparent)", outline: "none" },
  ".cm-tooltip": { backgroundColor: "var(--bg)", color: "var(--fg)", border: "1px solid var(--line)", borderRadius: "8px", overflow: "hidden" },
  ".cm-tooltip-autocomplete > ul > li": { padding: "2px 8px" },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--accent)", color: "var(--on-accent)" },
  ".cm-panels": { backgroundColor: "var(--side)", color: "var(--fg)" },
  // Find and replace: a find row (field, next, previous, all, toggles) over a replace row, in the app's pills.
  // Inline flow, not flex: the panel's <br> is what splits the rows, and a flex container ignores it.
  ".cm-panel.cm-search": { padding: "5px 40px 5px 10px", fontSize: "12px" },
  ".cm-search > *": { verticalAlign: "middle", margin: "3px 6px 3px 0" },
  ".cm-search .cm-textfield": {
    width: "220px", padding: "4px 12px", fontSize: "12px", color: "var(--fg)",
    border: "1px solid var(--line)", borderRadius: "999px", backgroundColor: "var(--bg)",
  },
  ".cm-search .cm-button": {
    padding: "3px 10px", fontSize: "12px", color: "var(--fg)", textTransform: "none",
    border: "1px solid var(--line)", borderRadius: "999px", backgroundImage: "none", backgroundColor: "var(--raised)",
  },
  ".cm-search .cm-button:hover": { backgroundColor: "var(--hover)" },
  ".cm-search .cm-button:active": { backgroundImage: "none", backgroundColor: "var(--press)" },
  ".cm-search label": { display: "inline-flex", alignItems: "center", gap: "4px", color: "var(--muted)", fontSize: "12px" },
  ".cm-search input[type=checkbox]": { margin: 0, accentColor: "var(--accent)" },
  ".cm-search button[name=close]": { top: "6px", right: "10px", margin: 0, color: "var(--muted)", fontSize: "18px" },
  ".cm-placeholder": { color: "var(--muted)" },
  ".cm-current-statement": { backgroundColor: "color-mix(in srgb, var(--accent) 7%, transparent)" },
});

/** SQL as highlighted text, for places that show SQL without editing it (the console, messages). */
export function highlightSql(text: string, kind: string): HTMLElement {
  const code = document.createElement("code");
  const parser = sql({ dialect: DIALECTS[kind as keyof typeof DIALECTS] ?? StandardSQL }).language.parser;
  const put = (part: string, classes: string) => {
    if (!classes) return void code.append(part);
    const span = document.createElement("span");
    span.className = classes;
    span.textContent = part;
    code.append(span);
  };
  highlightCode(text, parser.parse(text), highlight, put, () => code.append("\n"));
  return code;
}

/** A saved query offered as a completion when its keyword is typed. */
export type Snippet = { keyword: string; name: string; sql: string };
let snippets: Snippet[] = [];

/** Completes saved-query keywords. */
function snippetSource(cx: CompletionContext) {
  const word = cx.matchBefore(/\w+/);
  if (!snippets.length || !word || (word.from === word.to && !cx.explicit)) return null;
  const options = snippets.map((s) => ({ label: s.keyword, detail: s.name, info: s.sql, apply: s.sql, type: "text", boost: 50 }));
  return { from: word.from, options, validFor: /^\w*$/ };
}

// Built-in functions worth completing, per engine; the database's own come from the sidebar's items.
const COMMON_FUNCTIONS = ["avg", "count", "max", "min", "sum", "coalesce", "nullif", "lower", "upper", "length", "substring", "trim",
  "replace", "concat", "round", "floor", "ceil", "abs", "cast", "row_number", "rank", "dense_rank", "lag", "lead", "first_value", "last_value"];
const FUNCTIONS: Record<string, string[]> = {
  postgres: ["now", "current_date", "current_timestamp", "date_trunc", "date_part", "extract", "age", "to_char", "to_date", "to_timestamp",
    "string_agg", "array_agg", "json_agg", "jsonb_agg", "json_build_object", "jsonb_build_object", "jsonb_set", "to_jsonb",
    "generate_series", "unnest", "array_length", "split_part", "regexp_replace", "regexp_matches", "left", "right", "lpad", "rpad",
    "position", "greatest", "least", "gen_random_uuid", "nextval", "currval", "setval", "pg_size_pretty", "pg_total_relation_size"],
  mysql: ["now", "curdate", "curtime", "date_format", "date_add", "date_sub", "datediff", "timestampdiff", "str_to_date", "unix_timestamp",
    "from_unixtime", "group_concat", "ifnull", "if", "json_extract", "json_object", "json_arrayagg", "json_objectagg", "left", "right",
    "lpad", "rpad", "locate", "instr", "greatest", "least", "uuid", "last_insert_id", "found_rows", "format"],
  sqlite: ["date", "time", "datetime", "julianday", "strftime", "ifnull", "iif", "group_concat", "instr", "printf", "json_extract",
    "json_object", "json_group_array", "random", "last_insert_rowid", "changes", "typeof", "hex", "quote"],
};
FUNCTIONS.mariadb = FUNCTIONS.mysql;
/** The database's own functions and procedures, as the sidebar has loaded them. */
let databaseFunctions: () => string[] = () => [];

/** Table name → column names, as the backend's `schema` command lists them (Postgres tables are schema.table). */
export type Schema = Map<string, string[]>;

export type Editor = ReturnType<typeof createEditor>;
export type CompleteKey = "enter" | "tab" | "both";

/** The completion keymap with the chosen accept keys. Tab falls through to indenting when no completion is open. */
const completionKeys = (key: CompleteKey) => Prec.highest(keymap.of([
  ...completionKeymap.filter((b) => b.key !== "Enter"),
  ...(key !== "tab" ? [{ key: "Enter", run: acceptCompletion }] : []),
  ...(key !== "enter" ? [{ key: "Tab", run: acceptCompletion }] : []),
]));

/** The editors' look, from the theme's colours, for editors of other languages (scripts.ts). */
export const editorLook: Extension = [syntaxHighlighting(highlight), theme];

/** A small SQL editor for a definition (a view's SELECT, a function's CREATE): highlighting, brackets and
 *  undo, without the SQL tab's statement shading or completion. */
export function miniEditor(parent: HTMLElement, doc: string, kind: string, change: (doc: string) => void): EditorView {
  return new EditorView({
    parent,
    state: EditorState.create({
      doc,
      extensions: [
        basicSetup, keymap.of([indentWithTab]), sql({ dialect: DIALECTS[kind as keyof typeof DIALECTS] ?? StandardSQL }),
        syntaxHighlighting(highlight), theme, EditorView.updateListener.of((u) => u.docChanged && change(u.state.doc.toString())),
      ],
    }),
  });
}

/** The SQL editor. Each tab keeps its own EditorState (text + undo history); `setState` swaps them in. */
export function createEditor(
  parent: HTMLElement,
  on: { change: (doc: string) => void; cursor: () => void },
) {
  const language = new Compartment();
  const indent = new Compartment();
  const display = new Compartment();
  const binding = new Compartment();
  const completeKeys = new Compartment();
  const brackets = new Compartment();
  let lang: Extension = sql({ dialect: StandardSQL });
  let unit = "  ";
  let looks: Extension[] = [];
  let dialect: Dialect = "postgres";
  let schema: Schema = new Map();
  let upperKeywords = false;
  let vimMode: Extension = [];
  let accept: CompleteKey = "enter";
  /** basicSetup always closes brackets and quotes; an empty bracket list, ahead of every language's, turns it off. */
  const noClosing = Prec.highest(EditorState.languageData.of(() => [{ closeBrackets: { brackets: [] } }]));
  let closing: Extension = [];

  /** Completes built-in and database functions after a word of at least two letters. */
  const functionSource = (cx: CompletionContext) => {
    const word = cx.matchBefore(/\w+/);
    if (!word || (word.to - word.from < 2 && !cx.explicit)) return null;
    // Built-ins follow the keyword case; the database's own keep theirs, which may matter when quoted.
    const builtin = [...new Set([...(FUNCTIONS[dialect] ?? []), ...COMMON_FUNCTIONS])].map((n) => (upperKeywords ? n.toUpperCase() : n));
    const options = [...new Set([...builtin, ...databaseFunctions()])].map((label) => ({ label, type: "function", detail: "function" }));
    return { from: word.from, options, validFor: /^\w*$/ };
  };

  /** The statement under the cursor, shaded so it's clear what ⌘↩ will run. Off while text is selected. */
  const currentStatement = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = this.build(view);
      }
      update(u: ViewUpdate) {
        if (u.docChanged || u.selectionSet || u.focusChanged) this.decorations = this.build(u.view);
      }
      build(view: EditorView) {
        const b = new RangeSetBuilder<Decoration>();
        const sel = view.state.selection.main;
        const doc = view.state.doc;
        const s = sel.empty && doc.length < 200_000 ? statementAt(doc.toString(), sel.head, dialect) : undefined;
        // Only worth shading when there's more than one statement to tell apart.
        if (s && splitStatements(doc.toString(), dialect).length > 1) {
          for (let pos = doc.lineAt(s.from).from; pos <= s.to; ) {
            const line = doc.lineAt(pos);
            b.add(line.from, line.from, Decoration.line({ class: "cm-current-statement" }));
            pos = line.to + 1;
          }
        }
        return b.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );

  const extensions = () => [
    binding.of(vimMode), // before the other keymaps, so Vim sees keys first
    currentStatement,
    basicSetup,
    autocompletion({ defaultKeymap: false }), // completeKeys below instead, so Enter or Tab can accept
    completeKeys.of(completionKeys(accept)),
    brackets.of(closing),
    keymap.of([indentWithTab]), // Tab indents; Esc then Tab leaves the editor (CodeMirror's escape hatch)
    language.of(lang),
    indent.of(indentUnit.of(unit)),
    display.of(looks),
    syntaxHighlighting(highlight),
    placeholder("Write SQL here. Run runs the statement under the cursor (or the selection); Run all runs every statement."),
    theme,
    EditorState.languageData.of(() => [{ autocomplete: snippetSource }, { autocomplete: functionSource }]),
    EditorView.updateListener.of((u) => {
      if (u.docChanged) on.change(u.state.doc.toString());
      if (u.docChanged || u.selectionSet) on.cursor();
    }),
  ];

  const view = new EditorView({ parent, state: EditorState.create({ extensions: extensions() }) });
  // A stored state may carry an older dialect or indent; bring it up to date.
  const refresh = () =>
    view.dispatch({
      effects: [
        language.reconfigure(lang), indent.reconfigure(indentUnit.of(unit)), display.reconfigure(looks),
        binding.reconfigure(vimMode), completeKeys.reconfigure(completionKeys(accept)), brackets.reconfigure(closing),
      ],
    });

  const setLanguage = () => {
    const sqlDialect = DIALECTS[dialect as keyof typeof DIALECTS] ?? StandardSQL;
    if (dialect === "postgres") {
      // Nested by schema, so "public." completes tables and "people." completes columns.
      const nested: Record<string, Record<string, string[]>> = {};
      for (const [table, cols] of schema) {
        const [ns, name] = table.includes(".") ? table.split(/\.(.*)/s) : ["public", table];
        (nested[ns] ??= {})[name] = cols;
      }
      lang = sql({ dialect: sqlDialect, schema: nested, defaultSchema: "public", upperCaseKeywords: upperKeywords });
    } else {
      lang = sql({ dialect: sqlDialect, schema: Object.fromEntries(schema), upperCaseKeywords: upperKeywords });
    }
    refresh();
  };

  /** The lines the selection touches, or the whole text when nothing is selected. */
  const lineSpan = (from: number, to: number) => {
    const doc = view.state.doc;
    return from === to ? { from: 0, to: doc.length } : { from: doc.lineAt(from).from, to: doc.lineAt(to).to };
  };

  return {
    view,
    newState: (doc: string) => EditorState.create({ doc, extensions: extensions() }),
    setState(state: EditorState) {
      view.setState(state);
      refresh();
    },
    /** Dialect for highlighting, keywords and statement splitting; the schema feeds autocomplete. */
    setDialect(kind: string, tables: Schema) {
      kind = sqlKind(kind);
      dialect = (kind in DIALECTS ? kind : "postgres") as Dialect;
      schema = tables;
      setLanguage();
    },
    /** Completion settings (Settings › Editor): keywords in upper case, and the key that accepts a completion. */
    setCompletion(upper: boolean, key: CompleteKey) {
      upperKeywords = upper;
      accept = key;
      setLanguage();
    },
    /** Whether typing an opening bracket or quote adds the closing one (Settings › Editor). */
    setCloseBrackets(on: boolean) {
      closing = on ? [] : noClosing;
      refresh();
    },
    /** Where the database's own function names come from (the sidebar's loaded items). */
    setFunctionSource(names: () => string[]) {
      databaseFunctions = names;
    },
    /** Sublime-style keys (CodeMirror's defaults) or Vim. Vim's code loads the first time it's chosen. */
    async setKeyBinding(mode: "sublime" | "vim") {
      vimMode = mode === "vim" ? (await import("@replit/codemirror-vim")).vim() : [];
      refresh();
    },
    /** Applies a text transform to the selection (or the word at the cursor), or for whole-line transforms to the
     *  selected lines (or every line), as one undoable change. */
    transform(t: Transform) {
      const width = unit === "\t" ? 4 : unit.length;
      const state = view.state;
      view.dispatch(state.changeByRange((r) => {
        const span = byLine(t) ? lineSpan(r.from, r.to) : r.empty ? (state.wordAt(r.head) ?? r) : r;
        const text = transformText(state.sliceDoc(span.from, span.to), t, width);
        const keep = r.empty && !byLine(t) ? EditorSelection.cursor(r.head) : EditorSelection.range(span.from, span.from + text.length);
        return { changes: { from: span.from, to: span.to, insert: text }, range: keep };
      }));
      view.focus();
    },
    /** Shifts the selected lines one indent unit right or left. */
    shift(right: boolean) {
      (right ? indentMore : indentLess)(view);
      view.focus();
    },
    /** Inserts text at the cursor, replacing the selection. */
    insert(text: string) {
      view.dispatch(view.state.replaceSelection(text));
      view.focus();
    },
    /** The statement under the cursor, or the selection when there is one. */
    current(): string {
      const { from, to, head } = view.state.selection.main;
      if (from !== to) return view.state.sliceDoc(from, to);
      return statementAt(view.state.doc.toString(), head, dialect)?.text ?? "";
    },
    /** Reformats the selection, or everything when nothing is selected, as one undoable change. Throws when the
     *  SQL can't be parsed, leaving the text as it was. */
    format(style: FormatStyle) {
      const { from, to } = view.state.selection.main;
      const [a, b] = from === to ? [0, view.state.doc.length] : [from, to];
      const text = formatDialect(view.state.sliceDoc(a, b), {
        dialect: FORMAT_DIALECTS[dialect],
        tabWidth: unit === "\t" ? 2 : unit.length,
        useTabs: unit === "\t",
        keywordCase: "preserve",
        indentStyle: style,
      });
      view.dispatch({ changes: { from: a, to: b, insert: text }, selection: { anchor: a, head: a + text.length } });
    },
    /** Every statement in the selection, or in the whole text when nothing is selected. */
    all(): Statement[] {
      const { from, to } = view.state.selection.main;
      return splitStatements(from === to ? view.state.doc.toString() : view.state.sliceDoc(from, to), dialect);
    },
    /** Saved queries whose keyword completes to their SQL. */
    setSnippets(list: Snippet[]) {
      snippets = list;
    },
    /** Soft-wraps long lines; shows spaces and tabs. */
    setDisplay(wrap: boolean, invisibles: boolean) {
      looks = [...(wrap ? [EditorView.lineWrapping] : []), ...(invisibles ? [highlightWhitespace()] : [])];
      refresh();
    },
    /** The dialect statements are split in. */
    dialect: () => dialect,
    setIndent(text: string) {
      unit = text;
      refresh();
    },
    cursor() {
      const head = view.state.selection.main.head;
      const line = view.state.doc.lineAt(head);
      return { line: line.number, column: head - line.from + 1 };
    },
    focus: () => view.focus(),
  };
}
