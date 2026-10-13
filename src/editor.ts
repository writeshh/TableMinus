import { EditorView, basicSetup } from "codemirror";
import { Compartment, EditorState, Prec, type Extension } from "@codemirror/state";
import { keymap, placeholder } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import { HighlightStyle, indentUnit, syntaxHighlighting } from "@codemirror/language";
import { MariaSQL, MySQL, PostgreSQL, SQLite, StandardSQL, sql } from "@codemirror/lang-sql";
import { tags as t } from "@lezer/highlight";

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
  "&": { height: "100%", fontSize: "var(--editor-size, 13px)", color: "CanvasText", backgroundColor: "transparent" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--mono)", lineHeight: "var(--editor-line, 1.5)" },
  ".cm-content": { caretColor: "var(--accent)", padding: "8px 0" },
  ".cm-cursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "2px" },
  ".cm-gutters": { backgroundColor: "transparent", color: "var(--muted)", border: "none", paddingLeft: "4px" },
  ".cm-activeLine": { backgroundColor: "var(--hover)" },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "CanvasText" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
    { backgroundColor: "color-mix(in srgb, var(--accent) 32%, transparent)" },
  ".cm-matchingBracket": { backgroundColor: "color-mix(in srgb, var(--accent) 22%, transparent)", outline: "none" },
  ".cm-tooltip": { backgroundColor: "Canvas", color: "CanvasText", border: "1px solid var(--line)", borderRadius: "8px", overflow: "hidden" },
  ".cm-tooltip-autocomplete > ul > li": { padding: "2px 8px" },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--accent)", color: "white" },
  ".cm-panels": { backgroundColor: "var(--side)", color: "CanvasText" },
  ".cm-placeholder": { color: "var(--muted)" },
});

export type Editor = ReturnType<typeof createEditor>;

/** The SQL editor. Each tab keeps its own EditorState (text + undo history); `setState` swaps them in. */
export function createEditor(parent: HTMLElement, on: { change: (doc: string) => void; cursor: () => void; run: () => void }) {
  const language = new Compartment();
  const indent = new Compartment();
  let lang: Extension = sql({ dialect: StandardSQL });
  let unit = "  ";

  const extensions = () => [
    Prec.highest(keymap.of([{ key: "Mod-Enter", run: () => (on.run(), true) }])),
    basicSetup,
    keymap.of([indentWithTab]), // Tab indents; Esc then Tab leaves the editor (CodeMirror's escape hatch)
    language.of(lang),
    indent.of(indentUnit.of(unit)),
    syntaxHighlighting(highlight),
    placeholder("Write SQL here. Run the selection, or everything."),
    theme,
    EditorView.updateListener.of((u) => {
      if (u.docChanged) on.change(u.state.doc.toString());
      if (u.docChanged || u.selectionSet) on.cursor();
    }),
  ];

  const view = new EditorView({ parent, state: EditorState.create({ extensions: extensions() }) });
  // A stored state may carry an older dialect or indent; bring it up to date.
  const refresh = () => view.dispatch({ effects: [language.reconfigure(lang), indent.reconfigure(indentUnit.of(unit))] });

  return {
    view,
    newState: (doc: string) => EditorState.create({ doc, extensions: extensions() }),
    setState(state: EditorState) {
      view.setState(state);
      refresh();
    },
    /** Dialect for highlighting and keywords; table names feed autocomplete. */
    setDialect(kind: string, tables: string[]) {
      const dialect = DIALECTS[kind as keyof typeof DIALECTS] ?? StandardSQL;
      lang = sql({ dialect, schema: tables, upperCaseKeywords: false });
      refresh();
    },
    setIndent(text: string) {
      unit = text;
      refresh();
    },
    /** The selection, or the whole text when nothing is selected. */
    selectionOrAll() {
      const { from, to } = view.state.selection.main;
      return from === to ? view.state.doc.toString() : view.state.sliceDoc(from, to);
    },
    cursor() {
      const head = view.state.selection.main.head;
      const line = view.state.doc.lineAt(head);
      return { line: line.number, column: head - line.from + 1 };
    },
    focus: () => view.focus(),
  };
}
