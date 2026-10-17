// What the run bar and Messages say about statements run from a SQL tab; run.ts runs them.

import type { QueryResult } from "./results";

type StatusKind = "" | "busy" | "error" | "done";

export const fmt = (n: number) => n.toLocaleString();

/** One result's line in Messages. */
export const rowsText = (r: QueryResult) =>
  r.columns.length ? `${fmt(r.rows.length)} row${r.rows.length === 1 ? "" : "s"}${r.truncated ? " (more not loaded)" : ""}` : `${fmt(r.affected)} rows affected`;

/** The run bar once a run is over: why it stopped, else what a single statement did, else how many of several ran.
 *  `only` is the result a single statement showed. */
export function runSummary(r: {
  total: number; done: number; failed: number; affected: number; stopped: string; ms: number; only?: QueryResult;
}): [string, StatusKind] {
  const many = r.total > 1;
  if (r.stopped) return [r.stopped, r.failed ? "error" : ""];
  if (!many && r.only) {
    const limited = r.only.truncated ? ` (first ${fmt(r.only.rows.length)}; Load more, or raise the row limit)` : "";
    return [`${fmt(r.only.rows.length)} rows${limited} · ${r.ms} ms`, "done"];
  }
  if (!many) return [`${fmt(r.affected)} rows affected · ${r.ms} ms`, "done"];
  return [`${r.done} of ${r.total} statements${r.failed ? ` · ${r.failed} failed` : ""} · ${fmt(r.affected)} rows affected · ${r.ms} ms`, r.failed ? "error" : "done"];
}
