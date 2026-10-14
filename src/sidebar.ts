// The left sidebar's item tree: pinned items, item groups (by name rules), tables, and sections for views,
// materialized views, functions and procedures, sequences and types, each switchable on or off; a schema
// switcher on Postgres; "search in database"; and an item's overview. Table rows come from main.ts (they expand to
// columns and carry the table menu); everything else is drawn here.
import { el } from "./dom";
import { modal } from "./backup";

type MenuItem = [string, () => void] | "-";
type Kind = "schema" | "view" | "matview" | "function" | "procedure" | "sequence" | "type";
export type Item = { kind: Kind; name: string };
export type SearchMode = "contains" | "not_contains" | "starts_with" | "ends_with";
type Overview = { rows: number | null; estimated: boolean; bytes: number | null; created: string | null };

export type Ctx = {
  /** The active connection's name and engine. */
  conn: () => { name: string; kind: string } | null;
  /** Tables as the backend lists them (MySQL and SQLite include views; Postgres names are schema.table). */
  tables: () => string[];
  call: <T>(cmd: string, args: Record<string, unknown>) => Promise<T>;
  /** A table's row (expandable to its columns, with the table menu); `extra` joins that menu. */
  tableRow: (table: string, label: string, extra: MenuItem[]) => HTMLElement;
  openTable: (table: string) => void;
  /** Opens a table filtered to rows where any column matches, as search in database found them. */
  openFound: (table: string, mode: SearchMode, text: string) => void;
  menu: (e: MouseEvent, items: MenuItem[]) => void;
  /** Redraws the tree (main.ts's renderTables, which also updates the empty-list hint). */
  redraw: () => void;
};

let ctx: Ctx;
export const initSidebar = (c: Ctx) => void (ctx = c);

// ---------- Saved per connection: schema, pins, groups; per machine: which sections show ----------

type Rule = "prefix" | "suffix" | "contains" | "equals";
type Group = { name: string; rule: Rule; value: string };
type Prefs = { schema?: string | null; pins: string[]; groups: Group[] };
const RULES: [Rule, string][] = [["prefix", "Name starts with"], ["suffix", "Name ends with"], ["contains", "Name contains"], ["equals", "Name is"]];

const SECTIONS: [string, string, Kind[]][] = [
  ["views", "Views", ["view"]],
  ["matviews", "Materialized views", ["matview"]],
  ["functions", "Functions & procedures", ["function", "procedure"]],
  ["sequences", "Sequences", ["sequence"]],
  ["types", "Types", ["type"]],
];

function load<T>(key: string, fallback: T): T {
  try {
    return { ...fallback, ...JSON.parse(localStorage.getItem(key) ?? "{}") };
  } catch {
    return fallback;
  }
}
function store(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {} // unavailable storage: the choice lasts until the app closes
}

const prefsKey = () => `sidebar:${ctx.conn()?.name}`;
const prefs = (): Prefs => load<Prefs>(prefsKey(), { pins: [], groups: [] });
const savePrefs = (p: Prefs) => (store(prefsKey(), p), ctx.redraw());
/** Hidden sections and collapsed headings, the same for every connection. */
const looks = () => load<{ off: string[]; collapsed: string[] }>("sidebar-looks", { off: [], collapsed: [] });

// ---------- Items other than tables, loaded once per connection and database ----------

const items = new Map<string, Item[] | "loading">();

/** Forgets the loaded items (new database, reload, reconnect); the next draw loads them again. */
export const forgetItems = (conn?: string) => (conn ? items.delete(conn) : items.clear());

function itemsOf(conn: string): Item[] {
  const got = items.get(conn);
  if (got === "loading") return [];
  if (got) return got;
  items.set(conn, "loading");
  ctx
    .call<Item[]>("items", { name: conn })
    .then((list) => items.set(conn, Array.isArray(list) ? list : []))
    .catch(() => items.set(conn, [])) // the tables still show; the other sections stay empty
    .finally(() => ctx.conn()?.name === conn && ctx.redraw());
  return [];
}

/** The loaded items of a connection, for Open Anything (empty until the sidebar has loaded them). */
export const loadedItems = (conn: string): Item[] => {
  const got = items.get(conn);
  return Array.isArray(got) ? got : [];
};

// ---------- The tree ----------

/** Draws the tree into `host`; `query` is the sidebar's filter text. Returns how many items it shows. */
export function renderTree(host: HTMLElement, query: string): number {
  const conn = ctx.conn();
  if (!conn) return 0;
  const all = itemsOf(conn.name);
  const p = prefs();
  const pg = conn.kind === "postgres";
  const schemas = pg ? all.filter((i) => i.kind === "schema").map((i) => i.name) : [];
  renderSchemaPicker(schemas, p);
  const schema = pg && p.schema !== null && schemas.length ? (p.schema ?? (schemas.includes("public") ? "public" : null)) : null;
  // In one schema, names show without it; opening still uses the full name.
  const label = (name: string) => (schema && name.startsWith(`${schema}.`) ? name.slice(schema.length + 1) : name);
  const inSchema = (name: string) => !schema || name.startsWith(`${schema}.`);
  const q = query.toLowerCase();
  const shows = (name: string) => inSchema(name) && label(name).toLowerCase().includes(q);

  const viewNames = new Set(all.filter((i) => i.kind === "view" || i.kind === "matview").map((i) => i.name));
  const tables = ctx.tables().filter((t) => !viewNames.has(t) && shows(t));
  const others = all.filter((i) => i.kind !== "schema" && shows(i.name));
  const pinned = new Set(p.pins);
  const { off, collapsed } = looks();
  let count = 0;

  const rowFor = (name: string, kind: Kind | "table"): HTMLElement => {
    count++;
    const pin: MenuItem = pinned.has(name)
      ? ["Unpin", () => savePrefs({ ...p, pins: p.pins.filter((x) => x !== name) })]
      : ["Pin to top", () => savePrefs({ ...p, pins: [...p.pins, name] })];
    const common: MenuItem[] = ["-", pin, ["Copy name", () => void navigator.clipboard.writeText(name).catch(() => {})]];
    const browsable = kind === "table" || kind === "view" || kind === "matview";
    if (browsable) {
      const overview: MenuItem = ["Overview…", () => showOverview(name, row)];
      const row = ctx.tableRow(name, label(name), [overview, ...common]);
      return row;
    }
    const row = el("li", {}, el("div", { className: "tree-row", title: `${kind} ${name}` },
      el("span", { className: "chevron-gap" }), el("span", { className: `item-icon ${kind}` }), el("span", { className: "label", textContent: label(name) })));
    row.oncontextmenu = (e) => (e.stopPropagation(), ctx.menu(e, common.slice(1)));
    return row;
  };

  const section = (id: string, title: string, rows: HTMLElement[]) => {
    if (!rows.length && id !== "tables") return [];
    const open = !collapsed.includes(id);
    const head = el("button", { className: "section-head", textContent: `${title} ` },
      el("span", { className: "count", textContent: String(rows.length) }));
    head.setAttribute("aria-expanded", String(open));
    head.onclick = () => {
      const l = looks();
      store("sidebar-looks", { ...l, collapsed: open ? [...l.collapsed, id] : l.collapsed.filter((c) => c !== id) });
      ctx.redraw();
    };
    return [el("li", { className: "section" }, head, ...(open ? [el("ul", {}, ...rows)] : []))];
  };

  const kindOf = new Map(all.map((i) => [i.name, i.kind]));
  const pinRows = p.pins.filter(shows).filter((n) => kindOf.has(n) || ctx.tables().includes(n)).map((n) => rowFor(n, kindOf.get(n) ?? "table"));
  const unpinned = tables.filter((t) => !pinned.has(t));
  // Rules see the table's own name, without its schema, whichever schema is shown.
  const matches = (g: Group, t: string) => {
    const n = (pg ? t.slice(t.indexOf(".") + 1) : t).toLowerCase();
    const v = g.value.toLowerCase();
    return g.rule === "prefix" ? n.startsWith(v) : g.rule === "suffix" ? n.endsWith(v) : g.rule === "contains" ? n.includes(v) : n === v;
  };
  const grouped = new Set<string>();
  const groupSections = p.groups.flatMap((g, gi) => {
    const members = unpinned.filter((t) => !grouped.has(t) && matches(g, t));
    members.forEach((t) => grouped.add(t));
    const li = section(`group:${g.name}`, g.name, members.map((t) => rowFor(t, "table")));
    li[0]?.querySelector(".section-head")?.addEventListener("contextmenu", (e) => {
      e.stopPropagation();
      ctx.menu(e as MouseEvent, [
        ["Edit group…", () => editGroup(g, (ng) => savePrefs({ ...p, groups: p.groups.map((x, i) => (i === gi ? ng : x)) }))],
        ["Delete group", () => savePrefs({ ...p, groups: p.groups.filter((_, i) => i !== gi) })],
      ]);
    });
    return li;
  });

  host.replaceChildren(
    ...(pinRows.length ? section("pinned", "Pinned", pinRows) : []),
    ...groupSections,
    ...section("tables", "Tables", unpinned.filter((t) => !grouped.has(t)).map((t) => rowFor(t, "table"))),
    ...SECTIONS.flatMap(([id, title, kinds]) =>
      off.includes(id) ? [] : section(id, title, others.filter((i) => kinds.includes(i.kind) && !pinned.has(i.name)).map((i) => rowFor(i.name, i.kind))),
    ),
  );
  return count;
}

/** Postgres only: one schema at a time (or all). The choice is kept per connection. */
function renderSchemaPicker(schemas: string[], p: Prefs) {
  const picker = document.getElementById("schema-picker") as HTMLSelectElement;
  picker.hidden = schemas.length === 0;
  if (picker.hidden) return;
  const current = p.schema === null ? "" : (p.schema ?? (schemas.includes("public") ? "public" : ""));
  picker.replaceChildren(el("option", { value: "", textContent: "All schemas" }), ...schemas.map((s) => el("option", { value: s, textContent: s })));
  picker.value = schemas.includes(current) ? current : "";
  picker.onchange = () => savePrefs({ ...prefs(), schema: picker.value || null });
}

/** The sidebar's own right-click menu: new group, and which sections show. */
export function treeMenu(): MenuItem[] {
  const { off } = looks();
  const toggle = (id: string) => {
    const l = looks();
    store("sidebar-looks", { ...l, off: l.off.includes(id) ? l.off.filter((x) => x !== id) : [...l.off, id] });
    ctx.redraw();
  };
  return [
    ["New group…", () => editGroup({ name: "", rule: "prefix", value: "" }, (g) => savePrefs({ ...prefs(), groups: [...prefs().groups, g] }))],
    "-",
    ...SECTIONS.map(([id, title]) => [`${off.includes(id) ? "    " : "✓ "}${title}`, () => toggle(id)] as MenuItem),
  ];
}

/** A group's name and its rule; `save` gets the result unless cancelled. */
function editGroup(g: Group, save: (g: Group) => void) {
  const name = el("input", { value: g.name, placeholder: "e.g. Temporary" });
  const rule = el("select", {}, ...RULES.map(([v, t]) => el("option", { value: v, textContent: t })));
  rule.value = g.rule;
  const value = el("input", { value: g.value, placeholder: "e.g. tmp_", spellcheck: false });
  const form = el("form", { method: "dialog" }, el("fieldset", {}, el("label", {}, "Name ", name), el("label", {}, "Tables whose ", rule), el("label", {}, "Text ", value)));
  const { d, footer, setStatus } = modal(g.name ? `Edit ${g.name}` : "New group", "small", form);
  const cancel = el("button", { type: "button", textContent: "Cancel" });
  const ok = el("button", { type: "button", className: "primary", textContent: "Save" });
  footer.append(el("span", { className: "spacer" }), cancel, ok);
  cancel.onclick = () => d.close();
  ok.onclick = () => {
    if (!name.value.trim() || !value.value.trim()) return setStatus("Enter a name and the text to match.", "error");
    d.close();
    save({ name: name.value.trim(), rule: rule.value as Rule, value: value.value.trim() });
  };
  form.onsubmit = (e) => (e.preventDefault(), ok.click());
  d.showModal();
  name.focus();
}

// ---------- Overview popover ----------

const sizeFormat = (n: number) => {
  const units = ["bytes", "kB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) (n /= 1024), i++;
  return `${i ? n.toFixed(1) : n} ${units[i]}`;
};

async function showOverview(table: string, near: HTMLElement) {
  const conn = ctx.conn();
  if (!conn) return;
  document.querySelector(".overview")?.remove();
  const list = el("dl");
  const row = (k: string, v: string) => list.append(el("dt", { textContent: k }), el("dd", { textContent: v }));
  const pop = el("div", { className: "popover overview" }, el("h4", { textContent: table }), list);
  row("Rows", "Loading…");
  document.body.append(pop);
  const r = near.getBoundingClientRect();
  Object.assign(pop.style, { top: `${Math.min(r.top, innerHeight - 180)}px`, left: `${r.right + 8}px` });
  const close = (e: Event) => {
    if (e instanceof KeyboardEvent ? e.key === "Escape" : !pop.contains(e.target as Node)) {
      pop.remove();
      removeEventListener("pointerdown", close, true);
      removeEventListener("keydown", close, true);
    }
  };
  addEventListener("pointerdown", close, true);
  addEventListener("keydown", close, true);
  try {
    const o = await ctx.call<Overview>("item_overview", { name: conn.name, table });
    list.replaceChildren();
    row("Rows", o.rows === null ? "Unknown (not analyzed yet)" : `${o.estimated ? "about " : ""}${o.rows.toLocaleString()}`);
    row("Size", o.bytes === null ? "Unknown" : sizeFormat(o.bytes));
    if (o.created) row("Created", o.created);
  } catch (e) {
    list.replaceChildren();
    row("Error", String(e));
  }
}

// ---------- Search in database ----------

let searching: { stop: boolean } | null = null;

/** Wires the Search side pane: each table is searched in turn, so Stop takes effect after the current one. */
export function initSearch() {
  const input = document.getElementById("db-search-text") as HTMLInputElement;
  const mode = document.getElementById("db-search-mode") as HTMLSelectElement;
  const go = document.getElementById("db-search-go") as HTMLButtonElement;
  const status = document.getElementById("db-search-status")!;
  const results = document.getElementById("db-search-results")!;
  const run = async () => {
    if (searching) return void (searching.stop = true);
    const conn = ctx.conn();
    const text = input.value;
    if (!conn || !text) return;
    const run = (searching = { stop: false });
    const m = mode.value as SearchMode;
    const tables = ctx.tables();
    results.replaceChildren();
    go.textContent = "Stop";
    let found = 0;
    for (const [i, table] of tables.entries()) {
      if (run.stop || ctx.conn()?.name !== conn.name) break;
      status.textContent = `Searching ${i + 1} of ${tables.length}: ${table}…`;
      try {
        const n = await ctx.call<number>("search_table", { name: conn.name, table, mode: m, text });
        if (!n) continue;
        found++;
        const b = el("button", { className: "found", title: `Open ${table} with these rows` },
          el("span", { className: "label", textContent: table }), el("span", { className: "count", textContent: n >= 1000 ? "1,000+" : n.toLocaleString() }));
        b.onclick = () => ctx.openFound(table, m, text);
        results.append(b);
      } catch (e) {
        results.append(el("p", { className: "hint error-text", textContent: `${table}: ${e}` }));
      }
    }
    status.textContent = `${run.stop ? "Stopped. " : ""}${found ? `Found in ${found} table${found === 1 ? "" : "s"}.` : "No matches."}`;
    go.textContent = "Search";
    searching = null;
  };
  go.onclick = run;
  input.onkeydown = (e) => e.key === "Enter" && (e.preventDefault(), searching || run());
}
