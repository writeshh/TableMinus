// Line, bar and pie charts of a query result, drawn as plain SVG: in a SQL tab's result area (main.ts) and on
// metrics boards (metrics.ts). Colours are charts.css's --series-n, checked for colour-blind separation in both
// themes; every chart has a legend for two or more series and a tooltip on hover and arrow keys, and the grid
// (or a board's table widget) is its table view.

import { el } from "./dom";
import type { QueryResult } from "./results";
import {
  SLOTS, chartData, extent, fitSpec, formatNumber, niceTicks, numericColumns, pieSlices, xPositions,
  type ChartKind, type ChartSpec, type Data,
} from "./chartdata";
import "./charts.css";

const NS = "http://www.w3.org/2000/svg";
function svg<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number> = {}, ...kids: Node[]) {
  const n = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, String(v));
  n.append(...kids);
  return n;
}

const color = (slot: number | null) => (slot === null ? "var(--series-other)" : `var(--series-${slot + 1})`);
/** About how wide a character of chart text is, for fitting labels without measuring each one. */
const CHAR = 6.5;
const clip = (s: string, n: number) => (s.length > n ? s.slice(0, Math.max(1, n - 1)) + "…" : s);

type TipRow = { slot: number | null; mark: "line" | "rect"; name: string; value: string };
type Tip = { show: (x: number, y: number, title: string, rows: TipRow[]) => void; hide: () => void };

/** Draws `spec` over `r` into `host`, and again whenever `host` changes size. */
export function drawChart(host: HTMLElement, r: QueryResult, spec: ChartSpec) {
  spec = fitSpec(spec, r);
  const data = chartData(r, spec);
  const plot = el("div", { className: "chart-plot" });
  const tipBox = el("div", { className: "chart-tip", hidden: true });
  const wrap = el("div", { className: "chart" });
  host.replaceChildren(wrap);
  if (!data.series.length || !data.labels.length) {
    return wrap.append(el("p", { className: "hint empty", textContent: data.labels.length ? "No column of numbers to chart." : "No rows to chart." }));
  }
  const pie = spec.kind === "pie" ? pieSlices(data.labels, data.series[0].values) : [];
  if (spec.kind === "pie") {
    const total = pie.reduce((sum, s) => sum + s.value, 0);
    wrap.append(legend(pie.map((s) => ({ slot: s.slot, mark: "rect", name: s.label, value: `${formatNumber(s.value)} · ${percent(s.value, total)}` }))));
  } else if (data.series.length > 1) {
    wrap.append(legend(data.series.map((s, i) => ({ slot: i, mark: spec.kind === "line" ? "line" : "rect", name: s.name, value: "" }))));
  }
  wrap.append(plot, tipBox);

  const tip: Tip = {
    show(x, y, title, rows) {
      tipBox.replaceChildren(el("div", { className: "chart-tip-title", textContent: title }),
        ...rows.map((row) => el("div", { className: "chart-tip-row" }, key(row), el("strong", { textContent: row.value }), el("span", { textContent: row.name }))));
      tipBox.hidden = false;
      const box = wrap.getBoundingClientRect();
      const t = tipBox.getBoundingClientRect();
      const left = x + 14 + t.width > box.width ? x - 14 - t.width : x + 14;
      tipBox.style.left = `${Math.max(0, left)}px`;
      tipBox.style.top = `${Math.min(Math.max(0, y - t.height / 2), box.height - t.height)}px`;
    },
    hide: () => void (tipBox.hidden = true),
  };
  const draw = () => {
    if (!wrap.isConnected) return seen.disconnect();
    const { width, height } = plot.getBoundingClientRect();
    if (width < 60 || height < 60) return plot.replaceChildren();
    const offset = plot.offsetTop; // the plot's top inside wrap, where the tooltip is placed
    const at = { ...tip, show: (x: number, y: number, title: string, rows: TipRow[]) => tip.show(x, y + offset, title, rows) };
    plot.replaceChildren(
      spec.kind === "pie" ? pieChart(pie, width, height, at)
      : spec.kind === "line" ? lineChart(data, width, height, at)
      : barChart(data, width, height, at));
  };
  const seen = new ResizeObserver(draw);
  seen.observe(plot);
}

const percent = (v: number, total: number) => `${((v / total) * 100).toFixed(v / total < 0.1 ? 1 : 0)}%`;

/** The coloured key beside a series name: a short stroke for lines, a small square for bars and slices. */
function key(row: { slot: number | null; mark: "line" | "rect" }) {
  const k = el("span", { className: `chart-key ${row.mark}` });
  k.style.setProperty("--key", color(row.slot));
  return k;
}

function legend(items: TipRow[]) {
  return el("ul", { className: "chart-legend" },
    ...items.map((i) => el("li", {}, key(i), el("span", { textContent: i.name }), ...(i.value ? [el("span", { className: "muted", textContent: i.value })] : []))));
}

/** The value axis: gridlines and labels at round values. */
function valueAxis(ticks: number[], y: (v: number) => number, left: number, right: number) {
  return svg("g", { class: "chart-axis" },
    ...ticks.map((t) => svg("g", {},
      svg("line", { x1: left, x2: right, y1: y(t), y2: y(t), class: t === 0 ? "baseline" : "" }),
      Object.assign(svg("text", { x: left - 8, y: y(t), "text-anchor": "end", "dominant-baseline": "middle" }), { textContent: formatNumber(t) }))));
}

/** Labels along the bottom at the given x positions, as many as fit without overlapping. */
function labelAxis(labels: string[], x: (i: number) => number, left: number, bottom: number, room: number) {
  const n = labels.length;
  const longest = Math.min(16, labels.reduce((m, l) => Math.max(m, l.length), 1));
  const fit = Math.max(1, Math.floor(room / (longest * CHAR + 16)));
  const step = Math.ceil(n / fit);
  const g = svg("g", { class: "chart-axis" });
  for (let i = 0; i < n; i += step) {
    const text = clip(labels[i], 16);
    const half = (text.length * CHAR) / 2;
    const t = svg("text", { x: Math.min(x(i), room + left - half + 12), y: bottom + 16, "text-anchor": "middle" }); // the last one stays in view
    t.textContent = text;
    g.append(t);
  }
  return g;
}

type Frame = { left: number; top: number; right: number; bottom: number; y: (v: number) => number; ticks: number[] };

/** The plot area inside `w`×`h`, leaving room for the value labels on the left and the x labels below. */
function frame(lo: number, hi: number, w: number, h: number): Frame {
  const ticks = niceTicks(lo, hi);
  const left = 10 + ticks.reduce((m, t) => Math.max(m, formatNumber(t).length), 1) * CHAR;
  const [top, right, bottom] = [10, w - 12, h - 26];
  const [t0, t1] = [ticks[0], ticks.at(-1)!];
  return { left, top, right, bottom, ticks, y: (v) => bottom - ((v - t0) / (t1 - t0)) * (bottom - top) };
}

/** The pointer (or arrow keys, when the chart has focus) picks a label; `show(i)` draws its highlight and tooltip.
 *  `indexAt` gives the label under the pointer, or null for none. */
function hoverByIndex(s: SVGSVGElement, n: number, indexAt: (e: PointerEvent) => number | null, show: (i: number) => void, hide: () => void) {
  let current = -1;
  const pick = (i: number) => ((current = Math.min(n - 1, Math.max(0, i))), show(current));
  s.addEventListener("pointermove", (e) => {
    const i = indexAt(e);
    if (i === null) hide();
    else pick(i);
  });
  s.addEventListener("pointerleave", () => document.activeElement !== s && hide());
  s.addEventListener("focus", () => pick(current < 0 ? 0 : current));
  s.addEventListener("blur", hide);
  s.addEventListener("keydown", (e) => {
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (d) (e.preventDefault(), pick(current + d));
  });
}

const pointerX = (e: PointerEvent) => e.clientX - (e.currentTarget as Element).getBoundingClientRect().left;

const chartSvg = (w: number, h: number, label: string) =>
  svg("svg", { width: w, height: h, viewBox: `0 0 ${w} ${h}`, tabindex: 0, role: "img", "aria-label": label });

const tipRows = (d: Data, i: number, mark: "line" | "rect"): TipRow[] =>
  d.series.map((s, k) => ({ slot: k, mark, name: s.name, value: s.values[i] === null ? "NULL" : formatNumber(s.values[i]!) }));

function lineChart(d: Data, w: number, h: number, tip: Tip) {
  const [lo, hi] = extent(d.series) ?? [0, 1];
  const f = frame(lo, hi, w, h);
  const pos = xPositions(d.labels);
  const x = (i: number) => f.left + pos[i] * (f.right - f.left);
  const s = chartSvg(w, h, `Line chart of ${d.series.map((s) => s.name).join(", ")} by ${d.labels.length} points`);
  s.append(valueAxis(f.ticks, f.y, f.left, f.right), labelAxis(d.labels, x, f.left, f.bottom, f.right - f.left));
  d.series.forEach((series, k) => {
    let path = "";
    let last = -1;
    series.values.forEach((v, i) => {
      if (v === null) return;
      path += `${last === i - 1 && path ? "L" : "M"}${x(i).toFixed(1)},${f.y(v).toFixed(1)}`; // a NULL breaks the line
      last = i;
    });
    s.append(svg("path", { d: path, class: "chart-line", style: `stroke: ${color(k)}` }));
    if (last >= 0) s.append(svg("circle", { cx: x(last), cy: f.y(series.values[last]!), r: 4, class: "chart-dot", style: `fill: ${color(k)}` }));
  });
  const cross = svg("g", { class: "chart-cross", visibility: "hidden" });
  s.append(cross);
  hoverByIndex(s, d.labels.length,
    (e) => {
      const px = pointerX(e);
      let best = 0;
      for (let i = 1; i < pos.length; i++) if (Math.abs(x(i) - px) < Math.abs(x(best) - px)) best = i;
      return best;
    },
    (i) => {
      cross.replaceChildren(svg("line", { x1: x(i), x2: x(i), y1: f.top, y2: f.bottom }),
        ...d.series.flatMap((series, k) => series.values[i] === null ? [] : [svg("circle", { cx: x(i), cy: f.y(series.values[i]!), r: 4, class: "chart-dot", style: `fill: ${color(k)}` })]));
      cross.setAttribute("visibility", "visible");
      tip.show(x(i), f.top + 20, d.labels[i], tipRows(d, i, "line"));
    },
    () => (cross.setAttribute("visibility", "hidden"), tip.hide()));
  return s;
}

/** A bar from the baseline to `end`, its data end rounded and its base square. */
function barPath(x: number, w: number, base: number, end: number) {
  const r = Math.min(4, w / 2, Math.abs(base - end));
  const d = end < base ? r : -r; // from the data end back toward the base
  return `M${x},${base}V${end + d}Q${x},${end} ${x + r},${end}H${x + w - r}Q${x + w},${end} ${x + w},${end + d}V${base}Z`;
}

function barChart(d: Data, w: number, h: number, tip: Tip) {
  const [lo, hi] = extent(d.series) ?? [0, 1];
  const f = frame(Math.min(0, lo), Math.max(0, hi), w, h);
  const n = d.labels.length;
  const band = (f.right - f.left) / n;
  const k = d.series.length;
  const gap = k > 1 ? 2 : 0; // the surface gap between touching bars
  const bw = Math.max(1, Math.min(24, (band * 0.8 - gap * (k - 1)) / k));
  const group = bw * k + gap * (k - 1);
  const x = (i: number) => f.left + band * i + band / 2;
  const s = chartSvg(w, h, `Bar chart of ${d.series.map((s) => s.name).join(", ")} by ${n} categories`);
  const wash = svg("rect", { class: "chart-wash", y: f.top, height: f.bottom - f.top, width: band, visibility: "hidden" });
  s.append(wash, valueAxis(f.ticks, f.y, f.left, f.right), labelAxis(d.labels, x, f.left, f.bottom, f.right - f.left));
  const zero = f.y(0);
  d.series.forEach((series, j) => {
    const bars = svg("g", { style: `fill: ${color(j)}` });
    series.values.forEach((v, i) => {
      if (v === null || v === 0) return;
      bars.append(svg("path", { d: barPath(x(i) - group / 2 + j * (bw + gap), bw, zero, f.y(v)) }));
    });
    s.append(bars);
  });
  hoverByIndex(s, n, (e) => Math.floor((pointerX(e) - f.left) / band),
    (i) => {
      wash.setAttribute("x", String(f.left + band * i));
      wash.setAttribute("visibility", "visible");
      tip.show(x(i) + group / 2, f.top + 20, d.labels[i], tipRows(d, i, "rect"));
    },
    () => (wash.setAttribute("visibility", "hidden"), tip.hide()));
  return s;
}

function pieChart(slices: ReturnType<typeof pieSlices>, w: number, h: number, tip: Tip) {
  const s = chartSvg(w, h, `Pie chart of ${slices.length} slices`);
  const total = slices.reduce((sum, x) => sum + x.value, 0);
  const [cx, cy, r] = [w / 2, h / 2, Math.max(10, Math.min(w, h) / 2 - 8)];
  const point = (a: number) => `${(cx + r * Math.sin(a)).toFixed(2)},${(cy - r * Math.cos(a)).toFixed(2)}`;
  let a = 0;
  const paths = slices.map((slice) => {
    const sweep = (slice.value / total) * Math.PI * 2;
    const d = slices.length === 1
      ? `M${cx},${cy - r}A${r},${r} 0 1 1 ${cx - 0.01},${cy - r}Z`
      : `M${cx},${cy}L${point(a)}A${r},${r} 0 ${sweep > Math.PI ? 1 : 0} 1 ${point(a + sweep)}Z`;
    a += sweep;
    return svg("path", { d, class: "chart-slice", style: `fill: ${color(slice.slot)}` });
  });
  s.append(...paths);
  hoverByIndex(s, slices.length,
    (e) => {
      const i = paths.indexOf(e.target as SVGPathElement);
      return i < 0 ? null : i;
    },
    (i) => {
      paths.forEach((p, j) => p.classList.toggle("dim", j !== i));
      const box = paths[i].getBBox();
      tip.show(box.x + box.width / 2, box.y + box.height / 2, slices[i].label,
        [{ slot: slices[i].slot, mark: "rect", name: percent(slices[i].value, total), value: formatNumber(slices[i].value) }]);
    },
    () => (paths.forEach((p) => p.classList.remove("dim")), tip.hide()));
  return s;
}

// ---------- Pickers: chart type, x column, y columns ----------

const KINDS: [ChartKind, string][] = [["line", "Line"], ["bar", "Bar"], ["pie", "Pie"]];

/** The chart type, the x column and the y columns for `r`'s columns; `change` gets every new spec. */
export function chartPickers(r: QueryResult, spec: ChartSpec, change: (s: ChartSpec) => void, kinds = KINDS.map(([k]) => k)): HTMLElement {
  const box = el("div", { className: "chart-pickers" });
  const numeric = numericColumns(r);
  const render = () => {
    spec = fitSpec(spec, r);
    const set = (next: ChartSpec) => ((spec = next), change(spec), render());
    const seg = el("div", { className: "seg" },
      ...KINDS.filter(([k]) => kinds.includes(k)).map(([k, label]) => {
        const b = el("button", { type: "button", textContent: label });
        b.setAttribute("aria-pressed", String(spec.kind === k));
        b.onclick = () => set({ ...spec, kind: k });
        return b;
      }));
    const x = el("select", { title: "Labels along the x axis (slices of a pie)" },
      ...r.columns.map((c) => el("option", { value: c, textContent: c })));
    x.value = spec.x;
    x.onchange = () => set({ ...spec, x: x.value });
    const ys = numeric.filter((c) => c !== spec.x).map((c) => {
      const b = el("button", { type: "button", className: "chip", textContent: c });
      const on = spec.y.includes(c);
      b.setAttribute("aria-pressed", String(on));
      b.onclick = () => set({ ...spec, y: spec.kind === "pie" ? [c] : on ? spec.y.filter((y) => y !== c) : [...spec.y, c].slice(0, SLOTS) });
      return b;
    });
    box.replaceChildren(...(kinds.length > 1 ? [seg] : []),
      el("label", { className: "chart-x" }, "X", x),
      el("span", { className: "chart-ys" }, el("span", { className: "muted", textContent: spec.kind === "pie" ? "Value" : "Y" }),
        ...(ys.length ? ys : [el("span", { className: "muted", textContent: "no number columns" })])));
  };
  render();
  return box;
}

/** A result shown as a chart: the pickers above it and, with `add`, a button that puts it on a metrics board. */
export function chartPanel(r: QueryResult, spec: ChartSpec, on: { change: (s: ChartSpec) => void; add?: (s: ChartSpec) => void }) {
  const host = el("div", { className: "chart-host" });
  const bar = el("div", { className: "chart-bar" }, chartPickers(r, spec, (s) => ((spec = s), on.change(s), drawChart(host, r, s))));
  if (on.add) {
    const add = el("button", { type: "button", className: "ghost", textContent: "Add to Metrics Board" });
    add.onclick = () => on.add!(fitSpec(spec, r));
    bar.append(el("span", { className: "spacer" }), add);
  }
  const panel = el("div", { className: "chart-panel" }, bar, host);
  requestAnimationFrame(() => drawChart(host, r, spec)); // once it's in the page and has a size
  return panel;
}
