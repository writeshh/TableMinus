// Splits SQL into statements at semicolons that aren't inside strings, quoted names, comments or Postgres
// dollar quotes. Pure, so it's tested on its own (statements.test.ts).

export type Dialect = "postgres" | "mysql" | "mariadb" | "sqlite";
export type Statement = { from: number; to: number; text: string };

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
  let i = 0;
  while (i < sql.length) {
    const c = sql[i];
    const next = sql[i + 1];
    if (c === "-" && next === "-") i = lineEnd(sql, i);
    else if (c === "#" && mysql) i = lineEnd(sql, i);
    else if (c === "/" && next === "*") i = blockEnd(sql, i);
    else if (c === "'" || c === '"' || c === "`") i = quoteEnd(sql, i, c, mysql && c !== "`");
    else if (c === "$" && dialect === "postgres" && dollarTag(sql, i)) i = dollarEnd(sql, i, dollarTag(sql, i)!);
    else if (c === ";") {
      push(i);
      start = ++i;
    } else i++;
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
