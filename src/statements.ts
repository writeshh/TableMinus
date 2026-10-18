// Reads SQL the way the server will: splitting statements at semicolons and finding query parameters, both only
// outside strings, quoted names, comments and Postgres dollar quotes; and finding the one table a plain SELECT
// reads. Pure, so it's tested on its own (statements.test.ts).

export type Dialect = "postgres" | "mysql" | "mariadb" | "sqlite" | "duckdb";
export type Statement = { from: number; to: number; text: string };

/** The stretches of `sql` that are code: everything except strings, quoted names, comments and dollar quotes. */
export function codeRanges(sql: string, dialect: Dialect): [number, number][] {
  const mysql = dialect === "mysql" || dialect === "mariadb";
  const ranges: [number, number][] = [];
  let start = 0;
  let i = 0;
  const skip = (end: number) => {
    if (i > start) ranges.push([start, i]);
    start = i = end;
  };
  while (i < sql.length) {
    const c = sql[i];
    const next = sql[i + 1];
    if (c === "-" && next === "-") skip(lineEnd(sql, i));
    else if (c === "#" && mysql) skip(lineEnd(sql, i));
    else if (c === "/" && next === "*") skip(blockEnd(sql, i));
    else if (c === "'" || c === '"' || c === "`") skip(quoteEnd(sql, i, c, mysql && c !== "`"));
    else if (c === "$" && dialect === "postgres" && dollarTag(sql, i)) skip(dollarEnd(sql, i, dollarTag(sql, i)!));
    else i++;
  }
  if (i > start) ranges.push([start, i]);
  return ranges;
}

/** Every non-empty statement, with offsets into `sql` (trailing semicolon excluded, whitespace trimmed). */
export function splitStatements(sql: string, dialect: Dialect): Statement[] {
  const mysql = dialect === "mysql" || dialect === "mariadb";
  const out: Statement[] = [];
  let start = 0;
  const push = (end: number) => {
    const raw = sql.slice(start, end);
    const lead = raw.length - raw.trimStart().length;
    const text = raw.trim();
    if (text && !isOnlyComments(text, mysql)) out.push({ from: start + lead, to: start + lead + text.length, text });
  };
  for (const [from, to] of codeRanges(sql, dialect)) {
    for (let i = sql.indexOf(";", from); i >= 0 && i < to; i = sql.indexOf(";", i + 1)) {
      push(i);
      start = i + 1;
    }
  }
  push(sql.length);
  return out;
}

/** The statement the cursor is in; between statements, the one just before the cursor (else the next one). */
export function statementAt(sql: string, pos: number, dialect: Dialect): Statement | undefined {
  const all = splitStatements(sql, dialect);
  const inside = all.find((s) => pos >= s.from && pos <= s.to + 1); // +1: just after the semicolon still counts
  if (inside) return inside;
  const before = [...all].reverse().find((s) => s.to <= pos);
  return before ?? all.find((s) => s.from >= pos);
}

/** How query parameters are written; TablePlus's choices. */
export type ParamFormat = "colon" | "dollar" | "dollarBraces" | "doubleBraces" | "percent" | "question";

const PARAM_PATTERNS: Record<Exclude<ParamFormat, "question">, RegExp> = {
  colon: /(?<![:\w]):([A-Za-z_][\w.]*)/g, // not ::casts, and not a:b inside a word
  dollar: /\$([A-Za-z_][\w.]*)/g, // $1 stays a server-side placeholder
  dollarBraces: /\$\{([\w.]+)\}/g,
  doubleBraces: /\{\{([\w.]+)\}\}/g,
  percent: /%([\w.]+)%/g,
};

export type Param = { name: string; from: number; to: number };

/** Parameters in `sql` outside strings and comments. `?` parameters are named by position: "1", "2", … */
export function findParams(sql: string, dialect: Dialect, format: ParamFormat): Param[] {
  const found: Param[] = [];
  for (const [from, to] of codeRanges(sql, dialect)) {
    const code = sql.slice(from, to);
    if (format === "question") {
      for (let i = code.indexOf("?"); i >= 0; i = code.indexOf("?", i + 1)) {
        found.push({ name: String(found.length + 1), from: from + i, to: from + i + 1 });
      }
      continue;
    }
    for (const m of code.matchAll(PARAM_PATTERNS[format])) {
      found.push({ name: m[1], from: from + m.index, to: from + m.index + m[0].length });
    }
  }
  return found;
}

/** `sql` with each parameter replaced by its value, as typed (the user adds quotes for strings, as in TablePlus). */
export function fillParams(sql: string, params: Param[], values: Record<string, string>): string {
  let out = sql;
  for (const p of [...params].sort((a, b) => b.from - a.from)) out = out.slice(0, p.from) + (values[p.name] ?? "") + out.slice(p.to);
  return out;
}

const lineEnd = (sql: string, i: number) => {
  const n = sql.indexOf("\n", i);
  return n < 0 ? sql.length : n + 1;
};

const blockEnd = (sql: string, i: number) => {
  const n = sql.indexOf("*/", i + 2);
  return n < 0 ? sql.length : n + 2;
};

/** Past the closing quote. A doubled quote is an escaped quote; with `backslash`, so is a backslash-escaped one. */
function quoteEnd(sql: string, i: number, q: string, backslash: boolean) {
  let j = i + 1;
  while (j < sql.length) {
    if (backslash && sql[j] === "\\") j += 2;
    else if (sql[j] === q && sql[j + 1] === q) j += 2;
    else if (sql[j] === q) return j + 1;
    else j++;
  }
  return sql.length;
}

/** `$$` or `$tag$` starting at `i`, or null when the `$` is something else (e.g. a `$1` parameter). */
function dollarTag(sql: string, i: number): string | null {
  const m = /^\$([A-Za-z_][A-Za-z0-9_]*)?\$/.exec(sql.slice(i, i + 64));
  return m ? m[0] : null;
}

function dollarEnd(sql: string, i: number, tag: string) {
  const n = sql.indexOf(tag, i + tag.length);
  return n < 0 ? sql.length : n + tag.length;
}

function isOnlyComments(text: string, mysql: boolean) {
  const stripped = text
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(mysql ? /(--|#)[^\n]*/g : /--[^\n]*/g, "")
    .trim();
  return stripped === "";
}

/** The table a plain `select … from t [where …]` reads, as `tables` names it; null for joins, unions, grouping,
 *  subqueries and anything else whose rows don't map one to one onto a table's rows. */
export function sourceTable(sql: string, tables: string[]): string | null {
  const s = sql.replace(/--[^\n]*/g, "").replace(/\/\*[\s\S]*?\*\//g, "").trim();
  if (/\b(join|union|intersect|except|group\s+by|having|distinct|window)\b|\(\s*select\b/i.test(s)) return null;
  const m = /^select\b[\s\S]+?\bfrom\s+([`"]?[\w$]+[`"]?(?:\.[`"]?[\w$]+[`"]?)?)\s*(?:(?:as\s+)?\w+\s*)?(?:\b(?:where|order|limit|offset|fetch)\b[\s\S]*)?;?$/i.exec(s);
  if (!m) return null;
  const name = m[1].replace(/[`"]/g, "");
  const lower = name.toLowerCase();
  return (
    tables.find((t) => t === name) ??
    tables.find((t) => t === `public.${name}`) ??
    tables.find((t) => t.toLowerCase() === lower || t.toLowerCase().endsWith(`.${lower}`)) ??
    null
  );
}
