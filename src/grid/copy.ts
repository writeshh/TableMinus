// Turns grid cells into clipboard text: plain (tab-separated), CSV, a Markdown table, or SQL INSERT statements.
// Pure functions, so they're tested on their own (copy.test.ts).

export type Cells = { columns: string[]; rows: (string | null)[][] };
export type Dialect = "postgres" | "mysql" | "mariadb" | "sqlite" | "duckdb" | "mssql" | "clickhouse" | "oracle" | "bigquery";

/** Plain text: one cell as it is; several as tab-separated lines, quoting any value that holds a tab, newline or quote. */
export function plain(c: Cells): string {
  if (c.rows.length === 1 && c.rows[0].length === 1) return c.rows[0][0] ?? "NULL";
  const cell = (v: string | null) => (v === null ? "NULL" : /[\t\n\r"]/.test(v) ? `"${v.replaceAll('"', '""')}"` : v);
  return c.rows.map((r) => r.map(cell).join("\t")).join("\n");
}

/** RFC 4180 CSV with a header row; NULL becomes an empty field. */
export function csv(c: Cells): string {
  const cell = (v: string | null) => (v === null ? "" : /[",\n\r]/.test(v) ? `"${v.replaceAll('"', '""')}"` : v);
  return [c.columns, ...c.rows].map((r) => r.map(cell).join(",")).join("\n");
}

/** A Markdown table; pipes are escaped and line breaks become spaces so each row stays on one line. */
export function markdown(c: Cells): string {
  const cell = (v: string | null) => (v === null ? "NULL" : v.replaceAll("|", "\\|").replace(/\r?\n/g, " "));
  const line = (r: (string | null)[]) => `| ${r.map(cell).join(" | ")} |`;
  return [line(c.columns), `| ${c.columns.map(() => "---").join(" | ")} |`, ...c.rows.map(line)].join("\n");
}

function ident(name: string, d: Dialect): string {
  if (d === "mssql") return `[${name.replaceAll("]", "]]")}]`;
  if (d === "clickhouse") return `"${name.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"`; // a backslash escapes
  if (d === "bigquery") return `\`${name.replaceAll("\\", "\\\\").replaceAll("`", "\\`")}\``;
  return d === "mysql" || d === "mariadb" ? `\`${name.replaceAll("`", "``")}\`` : `"${name.replaceAll('"', '""')}"`;
}

/** A string literal. MySQL also treats backslash as an escape character by default, so it's doubled there. */
export function literal(v: string | null, d: Dialect): string {
  if (v === null) return "NULL";
  const s = v.replaceAll("'", "''");
  if (d === "mssql") return `N'${s}'`;
  if (d === "clickhouse") return `'${v.replaceAll("\\", "\\\\").replaceAll("'", "\\'")}'`;
  // A quoted string may not hold a line break either.
  if (d === "bigquery") return `'${v.replaceAll("\\", "\\\\").replaceAll("'", "\\'").replaceAll("\n", "\\n").replaceAll("\r", "\\r")}'`;
  return `'${d === "mysql" || d === "mariadb" ? s.replaceAll("\\", "\\\\") : s}'`;
}

/** One INSERT per row. `table` is `schema.table` for Postgres. Values are quoted strings; the server casts them. */
export function inserts(c: Cells, table: string, d: Dialect): string {
  const target = (d === "postgres" || d === "mssql" ? table.split(".") : [table]).map((p) => ident(p, d)).join(".");
  const cols = c.columns.map((n) => ident(n, d)).join(", ");
  return c.rows.map((r) => `INSERT INTO ${target} (${cols}) VALUES (${r.map((v) => literal(v, d)).join(", ")});`).join("\n");
}
