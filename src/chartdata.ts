// What a chart draws from a query result: which columns are numbers, the series, the axis ticks and pie slices.
// Pure, so it's tested on its own (chartdata.test.ts); charts.ts draws it.

import type { QueryResult } from "./results";

export type ChartKind = "line" | "bar" | "pie";
/** A chart of a result: `x` labels the points, each `y` column is a series (a pie uses the first). */
export type ChartSpec = { kind: ChartKind; x: string; y: string[] };

/** Categorical colours there are (charts.css); a result's further columns aren't drawn. */
export const SLOTS = 8;

const NUMBER = /^\s*[-+]?(\d+\.?\d*|\.\d+)(e[-+]?\d+)?\s*$/i;
export const toNumber = (v: string | null) => (v !== null && NUMBER.test(v) ? Number(v) : null);

/** Columns whose every value is a number or NULL, with at least one number. */
export function numericColumns(r: QueryResult): string[] {
  return r.columns.filter((_, c) => {
    let any = false;
    for (const row of r.rows) {
      const v = row[c];
      if (v === null) continue;
      if (toNumber(v) === null) return false;
      any = true;
    }
    return any;
  });
}

/** A starting chart: x is the first column that isn't a number (else the first), y the number columns besides it. */
export function guessSpec(r: QueryResult, kind: ChartKind = "bar"): ChartSpec {
  const numeric = numericColumns(r);
  const x = r.columns.find((c) => !numeric.includes(c)) ?? r.columns[0] ?? "";
  const y = numeric.filter((c) => c !== x);
  return { kind, x, y: kind === "pie" ? y.slice(0, 1) : y.slice(0, SLOTS) };
}

/** `spec` made to fit `r`: columns it doesn't have are dropped, and a chart left without x or y takes the guess. */
export function fitSpec(spec: ChartSpec, r: QueryResult): ChartSpec {
  const numeric = numericColumns(r);
  const guess = guessSpec(r, spec.kind);
  const x = r.columns.includes(spec.x) ? spec.x : guess.x;
  const y = spec.y.filter((c) => numeric.includes(c)).slice(0, spec.kind === "pie" ? 1 : SLOTS);
  return { kind: spec.kind, x, y: y.length ? y : guess.y };
}

export type Series = { name: string; values: (number | null)[] };
export type Data = { labels: string[]; series: Series[] };

export function chartData(r: QueryResult, spec: ChartSpec): Data {
  const xi = r.columns.indexOf(spec.x);
  const labels = r.rows.map((row) => (xi < 0 ? "" : (row[xi] ?? "NULL")));
  const series = spec.y.slice(0, SLOTS).map((name) => {
    const c = r.columns.indexOf(name);
    return { name, values: r.rows.map((row) => toNumber(row[c] ?? null)) };
  });
  return { labels, series };
}

/** The smallest and largest of the series' values; null when there are none. */
export function extent(series: Series[]): [number, number] | null {
  let [lo, hi] = [Infinity, -Infinity];
  for (const s of series) for (const v of s.values) if (v !== null) (lo = Math.min(lo, v)), (hi = Math.max(hi, v));
  return lo <= hi ? [lo, hi] : null;
}

/** Round tick values (steps of 1, 2 or 5 times a power of ten) covering `min`…`max`, about `count` steps. */
export function niceTicks(min: number, max: number, count = 5): number[] {
  if (min === max) [min, max] = min === 0 ? [0, 1] : min > 0 ? [0, min] : [min, 0];
  const raw = (max - min) / count;
  const pow = 10 ** Math.floor(Math.log10(raw));
  const f = raw / pow;
  const step = (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * pow;
  const start = Math.floor(min / step);
  const end = Math.ceil(max / step);
  const ticks: number[] = [];
  for (let i = start; i <= end; i++) ticks.push(Number((i * step).toPrecision(12))); // no 0.30000000000000004
  return ticks;
}

/** Where each label sits along a line chart's x axis, 0…1: by value when every label is a number or every one a
 *  date, else evenly in row order. */
export function xPositions(labels: string[]): number[] {
  const n = labels.length;
  const dated = labels.every((l) => /^\d{4}-\d{2}-\d{2}/.test(l));
  const values = dated
    ? labels.map((l) => Date.parse(l.replace(" ", "T"))) // WebKit doesn't parse "2024-01-02 10:00"
    : labels.every((l) => toNumber(l) !== null) ? labels.map(Number) : null;
  if (values && values.every(Number.isFinite)) {
    let [lo, hi] = [Infinity, -Infinity];
    for (const v of values) (lo = Math.min(lo, v)), (hi = Math.max(hi, v));
    if (hi > lo) return values.map((v) => (v - lo) / (hi - lo));
  }
  return labels.map((_, i) => (n === 1 ? 0.5 : i / (n - 1)));
}

export type Slice = { label: string; value: number; slot: number | null };

/** A pie's slices in row order: positive values only; past seven, all but the six largest fold into "Other"
 *  (slot null). Slot is the colour, by position, so a refresh keeps colours as long as the rows keep their order. */
export function pieSlices(labels: string[], values: (number | null)[], most = 7): Slice[] {
  const all = labels.map((label, i) => ({ label, value: values[i] ?? 0 })).filter((s) => s.value > 0);
  if (all.length <= most) return all.map((s, i) => ({ ...s, slot: i }));
  const keep = new Set([...all].sort((a, b) => b.value - a.value).slice(0, most - 1));
  const kept = all.filter((s) => keep.has(s)).map((s, i) => ({ ...s, slot: i as number | null }));
  const rest = all.filter((s) => !keep.has(s)).reduce((sum, s) => sum + s.value, 0);
  return [...kept, { label: "Other", value: rest, slot: null }];
}

const plain = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const compact = new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 });
/** 1,284 · 12.5 · 4.2M: commas up to a million, compact past it. */
export const formatNumber = (n: number) => (Math.abs(n) >= 1e6 ? compact : plain).format(n);
