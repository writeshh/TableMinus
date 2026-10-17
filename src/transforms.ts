// The SQL editor's text transforms (Edit menu): case changes, trimming trailing whitespace and converting
// indentation. Plain string functions, so they're tested without an editor; editor.ts applies them to the selection.

export type Transform = "upper" | "lower" | "capitalize" | "trim" | "tabs" | "spaces";

/** Each word with a capital first letter and the rest lower case. An apostrophe stays inside its word (it's). */
export const capitalize = (s: string) => s.toLowerCase().replace(/(^|[^\p{L}\p{N}_'’])(\p{L})/gu, (_, a: string, b: string) => a + b.toUpperCase());

export const trimTrailing = (s: string) => s.replace(/[ \t]+$/gm, "");

/** Rewrites each line's leading whitespace as tabs (plus spaces for a remainder) or as spaces, `width` columns a tab. */
export function convertIndent(s: string, toTabs: boolean, width: number) {
  return s.replace(/^[ \t]+/gm, (lead) => {
    let cols = 0;
    for (const c of lead) cols = c === "\t" ? cols - (cols % width) + width : cols + 1;
    return toTabs ? "\t".repeat(Math.floor(cols / width)) + " ".repeat(cols % width) : " ".repeat(cols);
  });
}

export function transformText(s: string, t: Transform, width: number) {
  switch (t) {
    case "upper": return s.toUpperCase();
    case "lower": return s.toLowerCase();
    case "capitalize": return capitalize(s);
    case "trim": return trimTrailing(s);
    case "tabs": return convertIndent(s, true, width);
    case "spaces": return convertIndent(s, false, width);
  }
}

/** Whether a transform works on whole lines (the selected lines, or every line) rather than on the selection or the
 *  word at the cursor. */
export const byLine = (t: Transform) => t === "trim" || t === "tabs" || t === "spaces";

export type ReplaceOptions = { matchCase: boolean; wholeCell: boolean };

/** Find and replace over a result's cells: [row, column, new value] for every cell that changes. NULL cells are
 *  skipped (there is no text to search). `find` is plain text, never a pattern. */
export function replaceCells(rows: (string | null)[][], find: string, replace: string, o: ReplaceOptions): [number, number, string][] {
  if (!find) return [];
  const text = find.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const re = new RegExp(o.wholeCell ? `^${text}$` : text, o.matchCase ? "gu" : "giu");
  const out: [number, number, string][] = [];
  rows.forEach((row, ri) =>
    row.forEach((v, ci) => {
      const next = v === null ? v : v.replace(re, () => replace); // a function, so $& in `replace` stays literal
      if (next !== null && next !== v) out.push([ri, ci, next]);
    }),
  );
  return out;
}
