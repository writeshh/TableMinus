// A table tab's sort cycle and what its footer and page settings say about paging; tabletab.ts draws them.

import { fmt } from "./run-text.ts"; // with the extension, for table-text.test.ts under node

type Sort = { column: string; desc: boolean };

/** Header click: ascending, then descending, then off. With `add` (Shift), the column joins the existing sort. */
export function nextSort(sort: Sort[], column: string, add: boolean): Sort[] {
  const i = sort.findIndex((s) => s.column === column);
  const cur = sort[i];
  const next = !cur ? { column, desc: false } : !cur.desc ? { column, desc: true } : null;
  if (!add) return next ? [next] : [];
  if (i < 0) return [...sort, next!];
  return next ? sort.map((s, j) => (j === i ? next : s)) : sort.filter((_, j) => j !== i);
}

type Page = { offset: number; shown: number; total?: number; estimated?: boolean };

/** The footer's "1–300 of ~12,000 rows"; "?" until counted. */
export function pageLabel({ offset, shown, total, estimated }: Page) {
  const of = total === undefined ? "?" : `${estimated ? "~" : ""}${fmt(total)}`;
  return `${shown ? fmt(offset + 1) : 0}–${fmt(offset + shown)} of ${of} rows`;
}

/** Whether Next has nowhere to go. An estimate can be off either way: a full page means there may be more. */
export const lastPage = ({ offset, shown, total, estimated }: Page, pageSize: number) =>
  shown < pageSize || (!estimated && total !== undefined && offset + shown >= total);

/** The page settings' Total row. */
export const totalLabel = (total: number | undefined, estimated: boolean | undefined) =>
  total === undefined ? "Not counted" : `${estimated ? "About " : ""}${fmt(total)} rows${estimated ? " (estimated)" : ""}`;
