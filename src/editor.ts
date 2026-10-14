import { EditorView, basicSetup } from "codemirror";
import { Compartment, EditorState, RangeSetBuilder, type Extension } from "@codemirror/state";
import { Decoration, ViewPlugin, highlightWhitespace, keymap, placeholder, type DecorationSet, type ViewUpdate } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import { HighlightStyle, indentUnit, syntaxHighlighting } from "@codemirror/language";
import { MariaSQL, MySQL, PostgreSQL, SQLite, StandardSQL, sql } from "@codemirror/lang-sql";
import { tags as t } from "@lezer/highlight";
import { statementAt, splitStatements, type Dialect, type Statement } from "./statements";
import { formatDialect, mariadb, mysql, postgresql, sqlite } from "sql-formatter";

const FORMAT_DIALECTS = { postgres: postgresql, mysql, mariadb, sqlite } as const;
export type FormatStyle = "standard" | "tabularLeft" | "tabularRight";

const DIALECTS = { postgres: PostgreSQL, mysql: MySQL, mariadb: MariaSQL, sqlite: SQLite } as const;

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
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--accent)", color: "white" },
  ".cm-panels": { backgroundColor: "var(--side)", color: "var(--fg)" },
  ".cm-placeholder": { color: "var(--muted)" },
  ".cm-current-statement": { backgroundColor: "color-mix(in srgb, var(--accent) 7%, transparent)" },
});

/** Table name → column names, as the backend's `schema` command lists them (Postgres tables are schema.table). */
export type Schema = Map<string, string[]>;

export type Editor = ReturnType<typeof createEditor>;

/** The SQL editor. Each tab keeps its own EditorState (text + undo history); `setState` swaps them in. */
export function createEditor(
  parent: HTMLElement,
  on: { change: (doc: string) => void; cursor: () => void },
) {
  const language = new Compartment();
  const indent = new Compartment();
  const display = new Compartment();
  let lang: Extension = sql({ dialect: StandardSQL });
  let unit = "  ";
  let looks: Extension[] = [];
  let dialect: Dialect = "postgres";

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
    currentStatement,
    basicSetup,
    keymap.of([indentWithTab]), // Tab indents; Esc then Tab leaves the editor (CodeMirror's escape hatch)
    language.of(lang),
    indent.of(indentUnit.of(unit)),
    display.of(looks),
    syntaxHighlighting(highlight),
    placeholder("Write SQL here. Run runs the statement under the cursor (or the selection); Run all runs every statement."),
    theme,
    EditorView.updateListener.of((u) => {
      if (u.docChanged) on.change(u.state.doc.toString());
      if (u.docChanged || u.selectionSet) on.cursor();
    }),
  ];

  const view = new EditorView({ parent, state: EditorState.create({ extensions: extensions() }) });
  // A stored state may carry an older dialect or indent; bring it up to date.
  const refresh = () =>
    view.dispatch({
      effects: [language.reconfigure(lang), indent.reconfigure(indentUnit.of(unit)), display.reconfigure(looks)],
    });

  return {
    view,
    newState: (doc: string) => EditorState.create({ doc, extensions: extensions() }),
    setState(state: EditorState) {
      view.setState(state);
      refresh();
    },
    /** Dialect for highlighting, keywords and statement splitting; the schema feeds autocomplete. */
    setDialect(kind: string, schema: Schema) {
      dialect = (kind in DIALECTS ? kind : "postgres") as Dialect;
      const sqlDialect = DIALECTS[kind as keyof typeof DIALECTS] ?? StandardSQL;
      if (kind === "postgres") {
        // Nested by schema, so "public." completes tables and "people." completes columns.
        const nested: Record<string, Record<string, string[]>> = {};
        for (const [table, cols] of schema) {
          const [ns, name] = table.includes(".") ? table.split(/\.(.*)/s) : ["public", table];
          (nested[ns] ??= {})[name] = cols;
        }
        lang = sql({ dialect: sqlDialect, schema: nested, defaultSchema: "public", upperCaseKeywords: false });
      } else {
        lang = sql({ dialect: sqlDialect, schema: Object.fromEntries(schema), upperCaseKeywords: false });
      }
      refresh();
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
