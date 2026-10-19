// Which history entries are kept and how the sidebar groups them; history.ts stores and draws them.

export type HistoryEntry = { sql: string; at: number; db: string | null };

const HISTORY_CAP = 5000;
const YEAR_MS = 365.25 * 864e5;

/** The entries newer than `years` (0 keeps them all). Anything but a list, as a hand-edited file may hold, is none. */
export function keptSince(list: unknown, years: number, now: number): HistoryEntry[] {
  const since = years ? now - years * YEAR_MS : 0;
  return Array.isArray(list) ? list.filter((h: HistoryEntry) => h.at >= since) : [];
}

/** `entry` first, the list capped at 5,000. */
export const prepend = (entry: HistoryEntry, list: HistoryEntry[]) => [entry, ...list].slice(0, HISTORY_CAP);

/** The entries whose SQL contains `query` (any case), grouped by `day(at)` in their order. */
export function byDay(list: HistoryEntry[], query: string, day: (at: number) => string) {
  const q = query.toLowerCase();
  const groups = new Map<string, HistoryEntry[]>();
  for (const h of list) if (h.sql.toLowerCase().includes(q)) groups.set(day(h.at), [...(groups.get(day(h.at)) ?? []), h]);
  return groups;
}
