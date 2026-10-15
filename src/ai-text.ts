// The AI assistant's text work, kept apart from the DOM so it can be tested: the schema and query results as the
// model reads them, an answer split into text and code blocks, and the query waiting for the user's answer.

export type Call = { id: string; sql: string };
export type Turn =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; call?: Call | null }
  | { kind: "result"; id: string; text: string };
export type QueryResult = { columns: string[]; rows: (string | null)[][]; affected: number; truncated: boolean };
export type DiagramData = {
  tables: { name: string; view: boolean; columns: { name: string; data_type: string; primary: boolean; nullable: boolean }[] }[];
  links: { table: string; columns: string[]; ref_table: string; ref_columns: string[] }[];
};

/** The schema text sent with each message. Large schemas are cut, saying how many tables were left out. */
export const MAX_SCHEMA = 60_000;
/** A query result as the model sees it. */
export const MAX_RESULT = 8_000;

/** The tables, their columns and types, and the foreign keys, as compact text, cut at `max` characters. */
export function schemaText(d: DiagramData, max = MAX_SCHEMA) {
  const keys = new Map<string, string[]>();
  for (const l of d.links) keys.set(l.table, [...(keys.get(l.table) ?? []), `(${l.columns.join(", ")}) → ${l.ref_table}(${l.ref_columns.join(", ")})`]);
  const lines: string[] = [];
  let size = 0;
  for (const [i, t] of d.tables.entries()) {
    const cols = t.columns.map((c) => `${c.name} ${c.data_type}${c.primary ? " primary key" : ""}${c.nullable ? "" : " not null"}`);
    const line = `${t.view ? "view " : ""}${t.name}(${cols.join(", ")})${keys.has(t.name) ? `; keys ${keys.get(t.name)!.join("; ")}` : ""}`;
    if (size + line.length > max) {
      lines.push(`…and ${d.tables.length - i} more tables, left out for length`);
      break;
    }
    lines.push(line);
    size += line.length + 1;
  }
  return lines.join("\n") || "(no tables)";
}

/** A query result as tab-separated text, cut at `max` characters. */
export function resultText(r: QueryResult, max = MAX_RESULT) {
  if (!r.columns.length) return `${r.affected} rows affected`;
  const cell = (v: string | null) => (v === null ? "NULL" : v.replace(/[\t\n\r]/g, " "));
  let text = r.columns.join("\t");
  let shown = 0;
  for (const row of r.rows) {
    const line = "\n" + row.map(cell).join("\t");
    if (text.length + line.length > max) break;
    text += line;
    shown++;
  }
  const more = shown < r.rows.length || r.truncated;
  return `${text}\n(${shown} row${shown === 1 ? "" : "s"}${more ? ", more not shown" : ""})`;
}

/** Splits an answer into text and fenced code blocks; `lang` is the fence's language, if any. */
export function blocks(text: string): { code: boolean; text: string; lang: string }[] {
  const out: { code: boolean; text: string; lang: string }[] = [];
  const fence = /```([\w-]*)[^\n]*\n([\s\S]*?)(?:```|$)/g;
  let at = 0;
  for (const m of text.matchAll(fence)) {
    if (m.index > at) out.push({ code: false, text: text.slice(at, m.index).trim(), lang: "" });
    out.push({ code: true, text: m[2].replace(/\n$/, ""), lang: m[1].toLowerCase() });
    at = m.index + m[0].length;
  }
  if (at < text.length) out.push({ code: false, text: text.slice(at).trim(), lang: "" });
  return out.filter((b) => b.code || b.text);
}

/** The call that waits for the user's answer: the last turn's, when no result follows it. */
export function pendingCall(turns: Turn[]): Call | null {
  const last = turns.at(-1);
  return last?.kind === "assistant" && last.call ? last.call : null;
}
