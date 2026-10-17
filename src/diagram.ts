// The ER diagram window (TablePlus's DiagramWindow): a schema's tables and views as boxes with their columns,
// foreign-key lines from referencing to referenced columns, drag to arrange, auto layout, zoom and pan, search,
// diagram files, and PNG/SVG export. Geometry and the file format live in diagram-layout.ts; the data comes from the
// `diagram` command (diagram.rs) in one round trip.

import { invoke } from "./ipc";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { connName, el } from "./dom";
import { modal } from "./backup";
import {
  HEAD, MAX_COLUMNS, ROW, boxSize, columnY, fromFile, layout, toFile, type Diagram, type Link, type Point, type Table,
} from "./diagram-layout";
import "./diagram.css";

type MenuItem = [string, () => void] | "-";
export type Ctx = {
  call: <T>(cmd: string, args: Record<string, unknown>) => Promise<T>;
  /** The on-screen connection: `name` is the session key commands take. */
  conn: () => { name: string; kind: string } | null;
  openTable: (table: string) => void;
  menu: (e: MouseEvent, items: MenuItem[]) => void;
};

let ctx: Ctx;
export const initDiagram = (c: Ctx) => void (ctx = c);

/** A schema with more tables than this starts with the table picker (or the focused table and its neighbours). */
const START_CAP = 80;
/** The most tables the diagram draws; past this dragging and zooming crawl. */
const MAX_SHOWN = 500;
const DIAGRAM_FILES = [{ name: "TableMinus diagram", extensions: ["json"] }];
const NS = "http://www.w3.org/2000/svg";

type View = { zoom: number; x: number; y: number };
/** One schema's diagram: where each shown table sits, and the zoom and scroll. */
type State = { schema: string | null; pos: Map<string, Point>; view: View; path?: string };
/** Kept per session and schema until the app quits, so closing the window or opening a table loses nothing. */
const states = new Map<string, State>();
const stateKey = (conn: string, schema: string | null) => `${conn}\u0000${schema ?? ""}`;

function svg<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number> = {}, ...kids: (Node | string)[]) {
  const n = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, String(v));
  n.append(...kids);
  return n;
}

// The canvas's styles sit inside the <svg>, so an SVG or PNG export carries them (with the colours resolved).
const SVG_STYLE = `
.dg-box { fill: var(--bg); stroke: var(--line); }
.dg-head { fill: var(--side); }
.dg-view .dg-box { stroke-dasharray: 5 3; }
.dg-title { font: 600 12px ui-monospace, "SF Mono", Menlo, monospace; fill: var(--fg); }
.dg-col, .dg-key, .dg-type, .dg-more { font: 12px ui-monospace, "SF Mono", Menlo, monospace; }
.dg-col { fill: var(--fg); }
.dg-key { fill: var(--warn); }
.dg-key.fk { fill: var(--accent); }
.dg-type, .dg-more { fill: var(--muted); }
.dg-link { fill: none; stroke: var(--muted); stroke-width: 1.3; }
.dg-link.hi { stroke: var(--accent); stroke-width: 2; }
.dg-end { fill: var(--muted); }
.dg-end.hi { fill: var(--accent); }
.dg-sel .dg-box { stroke: var(--accent); stroke-width: 2; }
.dg-match .dg-box { stroke: var(--warn); stroke-width: 2; }
.dg-dim { opacity: 0.35; }
`;
const COLOURS = ["--fg", "--bg", "--muted", "--line", "--accent", "--side", "--warn"];

/** Opens the diagram of `schema` (Postgres; null is the current schema, and other engines the current database).
 *  `focus` centres a table, and on a very large schema starts from it and its neighbours. */
export async function openDiagram(schema: string | null, focus?: string) {
  const conn = ctx.conn();
  if (!conn) return;
  const key = conn.name;
  const pg = conn.kind === "postgres";
  const label = (name: string) => (pg ? name.slice(name.indexOf(".") + 1) : name);

  let st: State = states.get(stateKey(key, schema)) ?? { schema, pos: new Map(), view: { zoom: 1, x: 40, y: 40 } };
  let data: Diagram = { tables: [], links: [] };
  let byName = new Map<string, Table>();
  let selected: string | null = null;
  let fresh = !states.has(stateKey(key, schema));

  // ---------- The window ----------
  const search = el("input", { type: "search", placeholder: "Find a table", className: "dg-search" });
  const found = el("datalist", { id: `dg-tables-${Date.now()}` });
  search.setAttribute("list", found.id);
  const zoomLabel = el("span", { className: "dg-zoom" });
  const count = el("span", { className: "dg-count" });
  const button = (text: string, title: string, run: () => void) => {
    const b = el("button", { type: "button", textContent: text, title });
    b.onclick = run;
    return b;
  };
  const toolbar = el("div", { className: "dg-toolbar" },
    button("Tables…", "Choose the tables on the diagram", () => pickTables()),
    button("Auto Layout", "Arrange every table again", () => (place(layout(shownTables(), data.links)), fit())),
    el("span", { className: "dg-group" },
      button("−", "Zoom out", () => zoomBy(1 / 1.25)), zoomLabel, button("+", "Zoom in", () => zoomBy(1.25)),
    ),
    button("Fit", "Zoom to fit every table", () => fit()),
    search, found, el("span", { className: "spacer" }), count,
  );
  const world = svg("g", { class: "dg-world" });
  const linksG = svg("g");
  const tablesG = svg("g");
  world.append(linksG, tablesG);
  const marker = (id: string, cls: string) =>
    svg("marker", { id, viewBox: "0 0 10 10", refX: 9, refY: 5, markerWidth: 8, markerHeight: 8, orient: "auto" },
      svg("path", { d: "M0 1 L10 5 L0 9 z", class: cls }));
  const dot = (id: string, cls: string) =>
    svg("marker", { id, viewBox: "0 0 10 10", refX: 5, refY: 5, markerWidth: 6, markerHeight: 6 }, svg("circle", { cx: 5, cy: 5, r: 4, class: cls }));
  const canvas = svg("svg", { class: "dg-canvas", tabindex: 0 },
    svg("style", {}, SVG_STYLE),
    svg("defs", {}, marker("dg-arrow", "dg-end"), marker("dg-arrow-hi", "dg-end hi"), dot("dg-dot", "dg-end"), dot("dg-dot-hi", "dg-end hi")),
    world,
  );
  const empty = el("div", { className: "dg-empty", textContent: "No tables on the diagram. Choose Tables… to add some." });
  const stage = el("div", { className: "dg-stage" }, canvas, empty);
  const { d, footer, setStatus } = modal(`Diagram · ${connName(key)}`, "tools-window diagram", toolbar, stage);
  const title = d.querySelector("h3")!;
  footer.append(
    button("Open…", "Open a diagram file", () => openFile()),
    button("Save…", "Save the tables and their positions to a file", () => saveFile()),
    button("Export PNG…", "Save the diagram as a picture", () => exportImage("png")),
    button("Export SVG…", "Save the diagram as a vector drawing", () => exportImage("svg")),
    el("span", { className: "spacer" }),
    Object.assign(button("Close", "", () => d.close()), { className: "primary" }),
  );
  d.addEventListener("close", () => states.set(stateKey(key, st.schema), st));
  d.showModal();
  canvas.focus();

  // ---------- Data ----------
  const shownTables = () => data.tables.filter((t) => st.pos.has(t.name));
  const related = (name: string) =>
    data.links.flatMap((l) => (l.table === name ? [l.ref_table] : l.ref_table === name ? [l.table] : [])).filter((n) => n !== name && byName.has(n));

  const fetchDiagram = (schemaName: string | null) => ctx.call<Diagram>("diagram", { name: key, schema: schemaName });
  function use(got: Diagram) {
    data = got;
    byName = new Map(got.tables.map((t) => [t.name, t]));
    found.replaceChildren(...got.tables.map((t) => el("option", { value: label(t.name) })));
  }

  // ---------- Drawing ----------
  function applyView() {
    world.setAttribute("transform", `translate(${st.view.x} ${st.view.y}) scale(${st.view.zoom})`);
    zoomLabel.textContent = `${Math.round(st.view.zoom * 100)}%`;
  }

  function linkPath(l: Link, i: number): string | null {
    const [a, b] = [byName.get(l.table), byName.get(l.ref_table)];
    const [pa, pb] = [st.pos.get(l.table), st.pos.get(l.ref_table)];
    if (!a || !b || !pa || !pb) return null;
    const [sa, sb] = [boxSize(a), boxSize(b)];
    const y1 = pa.y + columnY(a, l.columns[i]);
    const y2 = pb.y + columnY(b, l.ref_columns[i] ?? "");
    let [x1, x2, out1, out2] = [pa.x + sa.w, pb.x, 1, -1]; // referenced table to the right
    if (a === b) [x1, x2, out1, out2] = [pa.x + sa.w, pa.x + sa.w, 1, 1];
    else if (pb.x + sb.w < pa.x) [x1, x2, out1, out2] = [pa.x, pb.x + sb.w, -1, 1];
    else if (pb.x <= pa.x + sa.w) [x1, x2, out1, out2] = [pa.x, pb.x, -1, -1]; // overlapping columns: loop round the left
    const bend = Math.max(36, Math.abs(x2 - x1) / 2);
    return `M${x1} ${y1} C${x1 + out1 * bend} ${y1} ${x2 + out2 * bend} ${y2} ${x2} ${y2}`;
  }

  function drawLinks() {
    linksG.replaceChildren();
    for (const l of data.links) {
      const hi = selected !== null && (l.table === selected || l.ref_table === selected);
      l.columns.forEach((col, i) => {
        const path = linkPath(l, i);
        if (!path) return;
        const p = svg("path", {
          d: path, class: hi ? "dg-link hi" : "dg-link",
          "marker-start": hi ? "url(#dg-dot-hi)" : "url(#dg-dot)", "marker-end": hi ? "url(#dg-arrow-hi)" : "url(#dg-arrow)",
        }, svg("title", {}, `${l.name}: ${label(l.table)}.${col} → ${label(l.ref_table)}.${l.ref_columns[i] || "(primary key)"}`));
        if (hi) linksG.append(p);
        else linksG.prepend(p); // highlighted lines on top
      });
    }
  }

  function tableBox(t: Table, p: Point) {
    const { w, h } = boxSize(t);
    const fks = new Set(data.links.filter((l) => l.table === t.name).flatMap((l) => l.columns));
    const g = svg("g", { class: `dg-table${t.view ? " dg-view" : ""}`, transform: `translate(${p.x} ${p.y})` },
      svg("rect", { class: "dg-box", width: w, height: h, rx: 6 }),
      svg("rect", { class: "dg-head", x: 0.5, y: 0.5, width: w - 1, height: HEAD - 0.5, rx: 6 }),
      svg("rect", { class: "dg-head", x: 0.5, y: HEAD - 6, width: w - 1, height: 6 }),
      svg("text", { class: "dg-title", x: 10, y: 18 }, `${t.view ? "◇ " : ""}${label(t.name)}`, svg("title", {}, `${t.view ? "View" : "Table"} ${t.name}`)),
    );
    g.dataset.name = t.name;
    t.columns.slice(0, MAX_COLUMNS).forEach((c, i) => {
      const y = HEAD + i * ROW + 17;
      const fk = fks.has(c.name);
      g.append(
        svg("text", { class: fk && !c.primary ? "dg-key fk" : "dg-key", x: 8, y }, c.primary ? "⚿" : fk ? "↗" : ""),
        svg("text", { class: "dg-col", x: 24, y }, c.name,
          svg("title", {}, `${c.name} ${c.data_type}${c.nullable ? "" : " not null"}${c.primary ? ", primary key" : ""}${fk ? ", foreign key" : ""}`)),
        svg("text", { class: "dg-type", x: w - 10, y, "text-anchor": "end" }, c.data_type),
      );
    });
    if (t.columns.length > MAX_COLUMNS)
      g.append(svg("text", { class: "dg-more", x: 24, y: HEAD + MAX_COLUMNS * ROW + 17 }, `… ${t.columns.length - MAX_COLUMNS} more columns`));
    return g;
  }

  function draw() {
    const q = search.value.trim().toLowerCase();
    tablesG.replaceChildren(...shownTables().map((t) => {
      const g = tableBox(t, st.pos.get(t.name)!);
      if (t.name === selected) g.classList.add("dg-sel");
      if (q) g.classList.add(label(t.name).toLowerCase().includes(q) ? "dg-match" : "dg-dim");
      return g;
    }));
    drawLinks();
    applyView();
    const n = st.pos.size;
    count.textContent = `${n} of ${data.tables.length} ${data.tables.length === 1 ? "table" : "tables"}`;
    empty.hidden = n > 0;
  }

  // ---------- Placing, zooming, focusing ----------
  function bounds(names = [...st.pos.keys()]) {
    let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
    for (const n of names) {
      const [p, t] = [st.pos.get(n), byName.get(n)];
      if (!p || !t) continue;
      const { w, h } = boxSize(t);
      [x0, y0, x1, y1] = [Math.min(x0, p.x), Math.min(y0, p.y), Math.max(x1, p.x + w), Math.max(y1, p.y + h)];
    }
    return x0 === Infinity ? null : { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
  }

  function place(positions: Map<string, Point>) {
    st.pos = positions;
    draw();
  }

  /** Adds tables to the diagram, laid out among themselves to the right of what's there. */
  function addTables(names: string[]) {
    const add = names.filter((n) => byName.has(n) && !st.pos.has(n)).slice(0, Math.max(0, MAX_SHOWN - st.pos.size));
    if (!add.length) return;
    const b = bounds();
    const sub = layout(add.map((n) => byName.get(n)!), data.links);
    for (const [n, p] of sub) st.pos.set(n, b ? { x: b.x + b.w + 120 + p.x, y: b.y + p.y } : p);
    draw();
  }

  function removeTable(name: string) {
    st.pos.delete(name);
    if (selected === name) selected = null;
    draw();
  }

  function fit() {
    const b = bounds();
    if (!b) return;
    const { width, height } = canvas.getBoundingClientRect();
    const zoom = Math.min(Math.max(Math.min((width - 60) / b.w, (height - 60) / b.h, 1.25), 0.1), 3);
    st.view = { zoom, x: (width - b.w * zoom) / 2 - b.x * zoom, y: (height - b.h * zoom) / 2 - b.y * zoom };
    applyView();
  }

  function zoomAt(cx: number, cy: number, factor: number) {
    const zoom = Math.min(Math.max(st.view.zoom * factor, 0.1), 3);
    const k = zoom / st.view.zoom;
    st.view = { zoom, x: cx - (cx - st.view.x) * k, y: cy - (cy - st.view.y) * k };
    applyView();
  }

  function zoomBy(factor: number) {
    const { width, height } = canvas.getBoundingClientRect();
    zoomAt(width / 2, height / 2, factor);
  }

  /** Selects a table and scrolls it to the middle, zooming in if it would be too small to read. */
  function focusTable(name: string) {
    const [p, t] = [st.pos.get(name), byName.get(name)];
    if (!p || !t) return;
    selected = name;
    const { w, h } = boxSize(t);
    const { width, height } = canvas.getBoundingClientRect();
    const zoom = Math.max(st.view.zoom, 0.8);
    st.view = { zoom, x: width / 2 - (p.x + w / 2) * zoom, y: height / 2 - (p.y + h / 2) * zoom };
    draw();
  }

  // ---------- Pointer and keys ----------
  const local = (e: MouseEvent) => {
    const r = canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  const tableAt = (e: Event) => ((e.target as Element).closest?.(".dg-table") as SVGGElement | null)?.dataset.name ?? null;

  canvas.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const name = tableAt(e);
    const g = name ? (e.target as Element).closest(".dg-table") : null;
    const start = { x: e.clientX, y: e.clientY };
    const origin = name ? { ...st.pos.get(name)! } : { x: st.view.x, y: st.view.y };
    const gesture = new AbortController();
    let moved = false;
    canvas.addEventListener("pointermove", (m) => {
      const [dx, dy] = [m.clientX - start.x, m.clientY - start.y];
      if (!moved) {
        if (Math.hypot(dx, dy) < 3) return;
        // Captured only once it's a drag: a capture from the start would retarget the dblclick away from the table.
        moved = true;
        canvas.setPointerCapture(m.pointerId);
        canvas.classList.toggle("dg-panning", !name);
      }
      if (name && g) {
        const p = { x: origin.x + dx / st.view.zoom, y: origin.y + dy / st.view.zoom };
        st.pos.set(name, p);
        g.setAttribute("transform", `translate(${p.x} ${p.y})`);
        drawLinks();
      } else {
        st.view.x = origin.x + dx;
        st.view.y = origin.y + dy;
        applyView();
      }
    }, { signal: gesture.signal });
    const up = () => {
      gesture.abort();
      canvas.classList.remove("dg-panning");
      if (!moved && selected !== name) {
        selected = name;
        draw();
      }
    };
    addEventListener("pointerup", up, { signal: gesture.signal });
    addEventListener("pointercancel", up, { signal: gesture.signal });
  });

  canvas.addEventListener("dblclick", (e) => {
    const name = tableAt(e);
    if (!name) return;
    d.close();
    ctx.openTable(name);
  });

  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    // A trackpad pinch arrives as a wheel event with ctrlKey set; a plain wheel or two-finger scroll pans.
    if (e.ctrlKey || e.metaKey) {
      const { x, y } = local(e);
      zoomAt(x, y, Math.exp(-Math.max(-25, Math.min(25, e.deltaY)) * 0.01)); // a mouse wheel notch is ~100
    } else {
      st.view.x -= e.deltaX;
      st.view.y -= e.deltaY;
      applyView();
    }
  }, { passive: false });

  canvas.addEventListener("contextmenu", (e) => {
    const name = tableAt(e);
    if (!name) {
      ctx.menu(e, [["Tables…", () => pickTables()], ["Auto layout", () => (place(layout(shownTables(), data.links)), fit())], ["Fit to window", fit]]);
      return;
    }
    selected = name;
    draw();
    const more = related(name).filter((n) => !st.pos.has(n));
    ctx.menu(e, [
      ["Open table", () => (d.close(), ctx.openTable(name))],
      "-",
      [more.length ? `Add related tables (${more.length})` : "No related tables to add", () => addTables(more)],
      ["Remove from diagram", () => removeTable(name)],
    ]);
  });

  canvas.addEventListener("keydown", (e) => {
    if ((e.key === "Delete" || e.key === "Backspace") && selected) (e.preventDefault(), removeTable(selected));
    else if (e.key === "Enter" && selected) (d.close(), ctx.openTable(selected));
  });

  d.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "f") (e.preventDefault(), search.focus(), search.select());
  });

  // ---------- Search ----------
  let lastFound = -1;
  search.addEventListener("input", () => ((lastFound = -1), draw()));
  search.addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    e.preventDefault();
    const q = search.value.trim().toLowerCase();
    if (!q) return;
    const exact = data.tables.find((t) => label(t.name).toLowerCase() === q || t.name.toLowerCase() === q);
    const hits = shownTables().filter((t) => label(t.name).toLowerCase().includes(q));
    let target = exact && st.pos.has(exact.name) ? exact.name : hits.length ? hits[(lastFound = (lastFound + 1) % hits.length)].name : null;
    // A table that's in the schema but not on the diagram yet is added.
    const missing = exact ?? data.tables.find((t) => label(t.name).toLowerCase().includes(q));
    if (!target && missing) {
      addTables([missing.name]);
      target = st.pos.has(missing.name) ? missing.name : null;
    }
    if (target) focusTable(target);
    else setStatus(`No table matches “${search.value.trim()}”.`, "error");
  });

  // ---------- The table picker ----------
  function pickTables(note = "") {
    const chosen = new Set(st.pos.keys());
    const filter = el("input", { type: "search", placeholder: "Filter" });
    const list = el("div", { className: "dg-pick-list" });
    const counter = el("span", { className: "muted" });
    const ok = el("button", { type: "button", className: "primary", textContent: "Show" });
    const show = () => {
      const q = filter.value.trim().toLowerCase();
      list.replaceChildren(...data.tables.filter((t) => label(t.name).toLowerCase().includes(q)).map((t) => {
        const box = el("input", { type: "checkbox", checked: chosen.has(t.name) });
        box.onchange = () => ((box.checked ? chosen.add(t.name) : chosen.delete(t.name)), counted());
        return el("label", { className: "check" }, box, `${t.view ? "◇ " : ""}${label(t.name)}`);
      }));
      counted();
    };
    const counted = () => {
      counter.textContent = chosen.size > MAX_SHOWN ? `${chosen.size} chosen; the diagram shows up to ${MAX_SHOWN}` : `${chosen.size} of ${data.tables.length} chosen`;
      ok.disabled = chosen.size > MAX_SHOWN;
    };
    const every = (on: boolean) => () => {
      const q = filter.value.trim().toLowerCase();
      for (const t of data.tables) if (label(t.name).toLowerCase().includes(q)) on ? chosen.add(t.name) : chosen.delete(t.name);
      show();
    };
    const p = modal("Tables on the diagram", "tools-window dg-pick",
      ...(note ? [el("p", { className: "hint", textContent: note })] : []),
      el("div", { className: "dg-pick-bar" }, filter, button("All", "Choose every table the filter shows", every(true)), button("None", "Clear the tables the filter shows", every(false))),
      list,
    );
    const cancel = el("button", { type: "button", textContent: "Cancel" });
    cancel.onclick = () => p.d.close();
    ok.onclick = () => {
      p.d.close();
      for (const n of [...st.pos.keys()]) if (!chosen.has(n)) st.pos.delete(n);
      if (selected && !st.pos.has(selected)) selected = null;
      const added = data.tables.map((t) => t.name).filter((n) => chosen.has(n) && !st.pos.has(n));
      if (st.pos.size === 0) (place(layout(added.map((n) => byName.get(n)!), data.links)), fit());
      else addTables(added);
      draw();
    };
    p.footer.append(counter, el("span", { className: "spacer" }), cancel, ok);
    filter.oninput = show;
    show();
    p.d.showModal();
    filter.focus();
  }

  // ---------- Files ----------
  async function saveFile() {
    const path = await saveFileDialog({ defaultPath: st.path ?? `${connName(key)}${st.schema ? `-${st.schema}` : ""} diagram.json`, filters: DIAGRAM_FILES });
    if (!path) return;
    try {
      await invoke("write_text_file", { path, text: toFile(st.schema, st.pos) });
      st.path = path;
      setStatus(`Saved ${path}`, "done");
    } catch (e) {
      setStatus(String(e), "error");
    }
  }

  /** Reads and checks the whole file, and the schema it names, before anything on screen changes. */
  async function openFile() {
    const path = await openFileDialog({ multiple: false, directory: false, filters: DIAGRAM_FILES });
    if (typeof path !== "string") return;
    try {
      const file = fromFile(await invoke<string>("read_text_file", { path }));
      const schemaName = pg ? file.schema : null;
      const got = schemaName === st.schema ? data : await fetchDiagram(schemaName);
      const names = new Set(got.tables.map((t) => t.name));
      const pos = new Map(file.tables.filter((t) => names.has(t.name)).map((t) => [t.name, { x: t.x, y: t.y }]));
      const missing = file.tables.length - pos.size;
      // Everything's read: now switch. The diagram that was showing stays for next time.
      states.set(stateKey(key, st.schema), st);
      use(got);
      st = { schema: schemaName, pos, view: st.view, path };
      selected = null;
      showSchema();
      draw();
      fit();
      setStatus(missing ? `Opened; ${missing} ${missing === 1 ? "table isn't" : "tables aren't"} in the database and ${missing === 1 ? "was" : "were"} left out.` : `Opened ${path}`, missing ? "" : "done");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e), "error");
    }
  }

  /** The canvas as a standalone SVG of every table, colours resolved from the current theme. */
  function standaloneSvg() {
    const b = bounds();
    if (!b) return null;
    const m = 24;
    const [w, h] = [Math.ceil(b.w + m * 2), Math.ceil(b.h + m * 2)];
    const probe = el("span");
    canvas.parentElement!.append(probe);
    let style = SVG_STYLE;
    for (const v of COLOURS) {
      probe.style.color = `var(${v})`;
      style = style.replaceAll(`var(${v})`, getComputedStyle(probe).color);
    }
    probe.style.color = "var(--bg)";
    const background = getComputedStyle(probe).color;
    probe.remove();
    const copy = canvas.cloneNode(true) as SVGSVGElement;
    copy.setAttribute("xmlns", NS);
    copy.setAttribute("width", String(w));
    copy.setAttribute("height", String(h));
    copy.setAttribute("viewBox", `${b.x - m} ${b.y - m} ${w} ${h}`);
    copy.removeAttribute("tabindex");
    copy.querySelector("style")!.textContent = style;
    copy.querySelector(".dg-world")!.removeAttribute("transform");
    for (const g of copy.querySelectorAll(".dg-table")) g.classList.remove("dg-sel", "dg-match", "dg-dim");
    for (const p of copy.querySelectorAll(".hi")) p.classList.remove("hi");
    for (const p of copy.querySelectorAll("[marker-start]")) (p.setAttribute("marker-start", "url(#dg-dot)"), p.setAttribute("marker-end", "url(#dg-arrow)"));
    copy.insertBefore(svg("rect", { x: b.x - m, y: b.y - m, width: w, height: h, fill: background }), copy.querySelector(".dg-world"));
    return { text: new XMLSerializer().serializeToString(copy), w, h };
  }

  async function exportImage(kind: "png" | "svg") {
    const image = standaloneSvg();
    if (!image) return setStatus("There's nothing on the diagram to export.", "error");
    const path = await saveFileDialog({
      defaultPath: `${connName(key)}${st.schema ? `-${st.schema}` : ""} diagram.${kind}`,
      filters: [{ name: kind.toUpperCase(), extensions: [kind] }],
    });
    if (!path) return;
    try {
      setStatus("Exporting…", "busy");
      if (kind === "svg") await invoke("write_text_file", { path, text: image.text });
      else await invoke("write_image", { path, bytes: Array.from(await toPng(image)) });
      setStatus(`Exported ${path}`, "done");
    } catch (e) {
      setStatus(String(e), "error");
    }
  }

  // ---------- Start ----------
  function showSchema() {
    title.textContent = `Diagram · ${connName(key)}${pg ? ` · ${st.schema ?? "current schema"}` : ""}`;
  }
  showSchema();
  applyView();
  setStatus("Loading…", "busy");
  try {
    use(await fetchDiagram(schema));
  } catch (e) {
    return setStatus(String(e), "error");
  }
  setStatus("Drag tables to arrange them, drag the background to pan, pinch or ⌘-scroll to zoom; double-click a table to open it.");
  for (const n of [...st.pos.keys()]) if (!byName.has(n)) st.pos.delete(n); // dropped since last time
  if (fresh) {
    fresh = false;
    if (data.tables.length <= START_CAP) (place(layout(data.tables, data.links)), fit());
    else if (focus && byName.has(focus)) (place(layout([focus, ...related(focus)].map((n) => byName.get(n)!), data.links)), fit());
    else {
      draw();
      pickTables(`This schema has ${data.tables.length.toLocaleString()} tables; choose the ones to show.`);
    }
  } else draw();
  if (focus) focusTable(focus);
}

/** Renders the standalone SVG to PNG bytes at up to twice its size (less when that would pass canvas limits). */
async function toPng(image: { text: string; w: number; h: number }): Promise<Uint8Array> {
  const scale = Math.min(2, 16384 / image.w, 16384 / image.h, Math.sqrt(100e6 / (image.w * image.h)));
  const img = new Image();
  img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(image.text)}`;
  await img.decode();
  const c = el("canvas", { width: Math.round(image.w * scale), height: Math.round(image.h * scale) });
  c.getContext("2d")!.drawImage(img, 0, 0, c.width, c.height);
  const blob = await new Promise<Blob | null>((r) => c.toBlob(r, "image/png"));
  if (!blob) throw new Error("The picture couldn't be made; try fewer tables.");
  return new Uint8Array(await blob.arrayBuffer());
}
