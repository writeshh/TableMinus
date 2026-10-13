// Pure helpers for import and export: export file names from a pattern, and matching file fields to table
// columns. Tested on their own (transfer.test.ts).

export type NameParts = { connection: string; database: string; table: string; now: Date };

/**
 * A file name from `pattern`: {connection}, {database}, {table}, {date} (YYYY-MM-DD) and {time} (HH-MM-SS) are
 * filled in, characters no file system allows become "_", and `ext` is added.
 */
export function fileName(pattern: string, p: NameParts, ext: string): string {
  const two = (n: number) => String(n).padStart(2, "0");
  const d = p.now;
  const vars: Record<string, string> = {
    connection: p.connection,
    database: p.database,
    table: p.table,
    date: `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}`,
    time: `${two(d.getHours())}-${two(d.getMinutes())}-${two(d.getSeconds())}`,
  };
  const name = pattern.replace(/\{(\w+)\}/g, (m, k: string) => vars[k] ?? m).replace(/[/\\:*?"<>|\x00-\x1f]/g, "_").trim();
  return `${name || p.table || "export"}.${ext}`;
}

/** For each field, the column it goes into (null to skip): by name, or by position. */
export function matchColumns(fields: string[], columns: string[], byName: boolean, ignoreCase: boolean): (string | null)[] {
  if (!byName) return fields.map((_, i) => columns[i] ?? null);
  const norm = (s: string) => (ignoreCase ? s.toLowerCase() : s);
  return fields.map((f) => columns.find((c) => norm(c) === norm(f)) ?? null);
}
