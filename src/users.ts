// The user manager window: Postgres roles or MySQL/MariaDB accounts, their attributes, memberships and privileges
// (Rust: users.rs). Nothing runs until its SQL was previewed; the preview is the confirmation, also for safe mode.

import { el } from "./dom";
import { modal, type Confirm } from "./backup";
import { CANCELLED, guarded } from "./safety";
import "./users.css"; // vite injects it

type Obj =
  | { kind: "global" }
  | { kind: "database"; name: string }
  | { kind: "schema"; name: string }
  | { kind: "table"; schema: string; name: string };
type Grant = { on: Obj; privilege: string; passable: boolean };
type PgRole = {
  name: string; login: boolean; superuser: boolean; create_db: boolean; create_role: boolean; replication: boolean;
  bypass_rls: boolean; inherit: boolean; connection_limit: number; valid_until: string | null; member_of: string[];
  privileges: Grant[];
};
type MyUser = {
  user: string; host: string; plugin: string; max_queries: number; max_updates: number; max_connections: number;
  max_user_connections: number; locked: boolean; password_expired: boolean; password_lifetime: number | null; privileges: Grant[];
};
type Account = PgRole | MyUser;
type Accounts = { roles: PgRole[]; users: MyUser[]; objects: Obj[]; plugins: string[]; mariadb: boolean };

/** What the workspace lends this window: its session and its `call` (which shows the reconnect banner). */
export type UsersHost = {
  conn: { name: string; kind: string };
  call<T>(cmd: string, args: Record<string, unknown>): Promise<T>;
  confirm: Confirm;
};

/** The privileges offered per kind of object; ones the server reports beyond these show too. */
const PRIVILEGES: Record<"pg" | "my", Partial<Record<Obj["kind"], string[]>>> = {
  pg: {
    database: ["CONNECT", "CREATE", "TEMPORARY"],
    schema: ["USAGE", "CREATE"],
    table: ["SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER"],
  },
  my: {
    global: ["SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "RELOAD", "SHUTDOWN", "PROCESS", "FILE", "REFERENCES",
      "INDEX", "ALTER", "SHOW_DATABASES", "SUPER", "CREATE_TEMPORARY_TABLES", "LOCK_TABLES", "EXECUTE", "REPLICATION_SLAVE",
      "REPLICATION_CLIENT", "CREATE_VIEW", "SHOW_VIEW", "CREATE_ROUTINE", "ALTER_ROUTINE", "CREATE_USER", "EVENT", "TRIGGER",
      "CREATE_TABLESPACE"],
    database: ["SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "REFERENCES", "INDEX", "ALTER",
      "CREATE_TEMPORARY_TABLES", "LOCK_TABLES", "EXECUTE", "CREATE_VIEW", "SHOW_VIEW", "CREATE_ROUTINE", "ALTER_ROUTINE",
      "EVENT", "TRIGGER"],
    table: ["SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "REFERENCES", "INDEX", "ALTER", "CREATE_VIEW",
      "SHOW_VIEW", "TRIGGER"],
  },
};

const PG_FLAGS: [keyof PgRole, string][] = [
  ["login", "Can log in"], ["superuser", "Superuser"], ["create_db", "Create databases"], ["create_role", "Create roles"],
  ["replication", "Replication"], ["bypass_rls", "Bypass row-level security"], ["inherit", "Inherit privileges of its roles"],
];
const MY_LIMITS: [keyof MyUser, string][] = [
  ["max_queries", "Queries per hour"], ["max_updates", "Updates per hour"], ["max_connections", "Connections per hour"],
  ["max_user_connections", "Simultaneous connections"],
];

const objKey = (o: Obj) => JSON.stringify(o);
const objLabel = (o: Obj, my: boolean) =>
  o.kind === "global" ? "Global (*.*)"
  : o.kind === "table" ? `${o.schema}.${o.name}`
  : my && o.kind === "database" ? `${o.name}.*` : o.name;
const kindLabel: Record<Obj["kind"], string> = { global: "Global", database: "Database", schema: "Schema", table: "Table" };
const privLabel = (p: string) => p.replaceAll("_", " ");
const accountLabel = (a: Account) => ("user" in a ? `${a.user}@${a.host}` : a.name);
const clone = <T>(v: T): T => structuredClone(v);

/** The privileges per object, in the order objects first appear. */
type Rows = Map<string, { on: Obj; privs: Map<string, boolean> }>;
function toRows(grants: Grant[]): Rows {
  const rows: Rows = new Map();
  for (const g of grants) {
    const k = objKey(g.on);
    if (!rows.has(k)) rows.set(k, { on: g.on, privs: new Map() });
    rows.get(k)!.privs.set(g.privilege, g.passable);
  }
  return rows;
}
const toGrants = (rows: Rows): Grant[] =>
  [...rows.values()].flatMap((r) => [...r.privs].map(([privilege, passable]) => ({ on: r.on, privilege, passable })));

export function openUsers(host: UsersHost) {
  const { conn, call, confirm } = host;
  const my = conn.kind !== "postgres";
  const engine = my ? "my" : "pg";

  const list = el("div", { className: "user-list" });
  const editor = el("div", { className: "user-editor" });
  const newButton = el("button", { type: "button", textContent: my ? "New user" : "New role" });
  const { d, footer, setStatus } = modal(`${my ? "Users" : "Roles"} on ${conn.name.split("\u001f")[0]}`, "tools-window users",
    el("div", { className: "users-body" }, el("div", { className: "user-side" }, list, newButton), editor));
  const drop = el("button", { type: "button", className: "danger", textContent: "Drop…" });
  const revert = el("button", { type: "button", textContent: "Revert" });
  const save = el("button", { type: "button", className: "primary", textContent: "Save…" });
  const close = el("button", { type: "button", textContent: "Close" });
  footer.append(drop, el("span", { className: "spacer" }), close, revert, save);

  let data: Accounts = { roles: [], users: [], objects: [], plugins: [], mariadb: false };
  /** As loaded (null for a new account) and as edited. */
  let before: Account | null = null;
  let draft: Account | null = null;
  let rows: Rows = new Map();
  const password = el("input", { type: "password", autocomplete: "new-password" });

  const accounts = (): Account[] => (my ? data.users : data.roles.filter((r) => !r.name.startsWith("pg_")));
  /** The edited account; a blank "valid until" means never. */
  const current = (): Account | null => {
    if (!draft) return null;
    const a = { ...draft, privileges: toGrants(rows) };
    if ("valid_until" in a && !a.valid_until?.trim()) a.valid_until = null;
    return a;
  };
  const dirty = () => !!draft && (before === null || password.value !== "" || JSON.stringify(current()) !== JSON.stringify(before));

  async function load(select?: string) {
    setStatus("Loading…", "busy");
    try {
      data = await call<Accounts>("users", { name: conn.name });
      setStatus("");
    } catch (e) {
      setStatus(String(e), "error");
    }
    renderList();
    const pick = accounts().find((a) => accountLabel(a) === select) ?? null;
    if (pick) await show(pick);
    else clearEditor();
  }

  function clearEditor() {
    before = draft = null;
    rows = new Map();
    editor.replaceChildren(el("p", { className: "hint", textContent: accounts().length ? "Choose an account, or make a new one." : "No accounts to show." }));
    syncButtons();
  }

  function renderList() {
    const selected = before && accountLabel(before);
    list.replaceChildren(...accounts().map((a) => {
      const b = el("button", { type: "button", textContent: accountLabel(a), className: accountLabel(a) === selected ? "selected" : "" });
      b.onclick = async () => (await leave()) && show(a);
      return b;
    }));
  }

  /** Whether unsaved edits may be thrown away. */
  const leave = async () => !dirty() || confirm("Discard the changes to this account?", "Discard");

  async function show(a: Account) {
    let privileges: Grant[] = [];
    try {
      privileges = await call<Grant[]>("user_privileges", { name: conn.name, user: "user" in a ? a.user : a.name, host: "user" in a ? a.host : null });
    } catch (e) {
      setStatus(String(e), "error");
    }
    before = { ...clone(a), privileges };
    edit(clone(before));
  }

  function newAccount() {
    before = null;
    edit(my
      ? { user: "", host: "%", plugin: "", max_queries: 0, max_updates: 0, max_connections: 0, max_user_connections: 0,
        locked: false, password_expired: false, password_lifetime: null, privileges: [] }
      : { name: "", login: true, superuser: false, create_db: false, create_role: false, replication: false, bypass_rls: false,
          inherit: true, connection_limit: -1, valid_until: null, member_of: [], privileges: [] });
    (editor.querySelector("input") as HTMLInputElement | null)?.focus();
  }

  function edit(a: Account) {
    draft = a;
    rows = toRows(a.privileges);
    password.value = "";
    password.placeholder = before ? "Unchanged" : my ? "None" : "None (can't sign in with a password)";
    editor.replaceChildren(...("user" in a ? myForm(a) : pgForm(a)), privilegesSection());
    renderList();
    syncButtons();
  }

  const field = (label: string, input: HTMLElement, hint?: string) =>
    el("label", { className: "field" }, label, input, ...(hint ? [el("span", { className: "hint", textContent: hint })] : []));
  const text = <A,>(a: A, key: keyof A, attrs: Partial<HTMLInputElement> = {}) => {
    const i = el("input", { value: String(a[key] ?? ""), spellcheck: false, ...attrs });
    i.oninput = () => {
      (a[key] as unknown) = i.type === "number" ? Number(i.value) : i.value;
      syncButtons();
    };
    return i;
  };

  function pgForm(r: PgRole): Node[] {
    const flags = PG_FLAGS.map(([key, label]) => {
      const c = el("input", { type: "checkbox", checked: r[key] as boolean });
      c.onchange = () => (((r[key] as boolean) = c.checked), syncButtons());
      return el("label", { className: "check" }, c, ` ${label}`);
    });
    const others = data.roles.filter((o) => o.name !== (before as PgRole | null)?.name && o.name !== r.name);
    const members = others.map((o) => {
      const c = el("input", { type: "checkbox", checked: r.member_of.includes(o.name) });
      c.onchange = () => {
        r.member_of = c.checked ? [...r.member_of, o.name] : r.member_of.filter((m) => m !== o.name);
        syncButtons();
      };
      return el("label", { className: "check" }, c, ` ${o.name}`);
    });
    return [
      el("div", { className: "user-fields" },
        field("Name", text(r, "name")),
        field("Password", password, "Sent to the server as a SCRAM-SHA-256 hash."),
        field("Connection limit", text(r, "connection_limit", { type: "number", min: "-1" }), "-1 for no limit"),
        field("Valid until", text(r, "valid_until", { placeholder: "Never" }), "When the password stops working, e.g. 2030-01-01"),
      ),
      el("h4", { textContent: "Attributes" }), el("div", { className: "user-flags" }, ...flags),
      el("h4", { textContent: "Member of" }),
      members.length ? el("div", { className: "user-members" }, ...members) : el("p", { className: "hint", textContent: "No other roles." }),
    ];
  }

  function myForm(u: MyUser): Node[] {
    const plugin = el("select");
    const names = [...new Set([...data.plugins, u.plugin].filter(Boolean))];
    if (!before) plugin.append(el("option", { value: "", textContent: "Server default" }));
    plugin.append(...names.map((p) => el("option", { value: p, textContent: p })));
    plugin.value = u.plugin;
    plugin.onchange = () => ((u.plugin = plugin.value), syncButtons());
    const limits = MY_LIMITS.map(([key, label]) => field(label, text(u, key, { type: "number", min: "0" })));
    return [...accountFields(u, plugin, limits), ...(data.mariadb ? [] : lifecycleFields(u))];
  }

  /** MySQL's account lock and password policy (MariaDB keeps them elsewhere, so they aren't offered there). */
  function lifecycleFields(u: MyUser): Node[] {
    const wasExpired = !!(before as MyUser | null)?.password_expired;
    const locked = el("input", { type: "checkbox", checked: u.locked });
    locked.onchange = () => ((u.locked = locked.checked), syncButtons());
    const mode = el("select", {}, el("option", { value: "default", textContent: "Server default" }),
      el("option", { value: "never", textContent: "Never expires" }), el("option", { value: "days", textContent: "Expires every…" }));
    mode.style.minWidth = "11em";
    const days = el("input", { type: "number", min: "1", value: String(u.password_lifetime || 90) });
    const showMode = () => {
      mode.value = u.password_lifetime === null ? "default" : u.password_lifetime === 0 ? "never" : "days";
      days.hidden = mode.value !== "days";
    };
    const setLifetime = () => {
      u.password_lifetime = mode.value === "default" ? null : mode.value === "never" ? 0 : Math.max(1, Math.floor(Number(days.value)) || 1);
      showMode();
      syncButtons();
    };
    mode.onchange = setLifetime;
    days.oninput = setLifetime;
    showMode();
    const expired = el("input", { type: "checkbox", checked: u.password_expired, disabled: wasExpired });
    expired.onchange = () => ((u.password_expired = expired.checked), syncButtons());
    return [
      el("h4", { textContent: "Sign-in" }),
      el("label", { className: "check" }, locked, " Account locked"),
      el("div", { className: "user-fields" }, field("Password lifetime", el("div", { className: "opt-row" }, mode, days), "Days; 'Server default' follows default_password_lifetime.")),
      el("label", { className: "check" }, expired, " Ask for a new password at the next sign-in"),
      ...(wasExpired ? [el("p", { className: "hint", textContent: "The password has expired; setting a new one clears that." })] : []),
    ];
  }

  function accountFields(u: MyUser, plugin: HTMLSelectElement, limits: Node[]): Node[] {
    return [
      el("div", { className: "user-fields" },
        field("User", text(u, "user")),
        field("Host", text(u, "host"), "% for any host, e.g. 10.0.% or localhost"),
        field("Password", password),
        field("Authentication", plugin, before ? "Changing it needs a new password." : undefined),
      ),
      el("h4", { textContent: "Resource limits (0 for none)" }), el("div", { className: "user-limits" }, ...limits),
    ];
  }

  function privilegesSection() {
    const body = el("div", { className: "privilege-rows" });
    const picker = el("select");
    const add = el("button", { type: "button", textContent: "Add" });
    const fill = () => {
      const taken = new Set(rows.keys());
      const groups = new Map<string, HTMLOptGroupElement>();
      picker.replaceChildren(el("option", { value: "", textContent: "Grant on…" }));
      data.objects.forEach((o, i) => {
        if (taken.has(objKey(o)) || !PRIVILEGES[engine][o.kind]) return;
        if (!groups.has(o.kind)) picker.append(groups.set(o.kind, el("optgroup", { label: kindLabel[o.kind] })).get(o.kind)!);
        groups.get(o.kind)!.append(el("option", { value: String(i), textContent: objLabel(o, my) }));
      });
    };
    const render = () => {
      body.replaceChildren(...[...rows].map(([k, row]) => privilegeRow(k, row, render)));
      if (!rows.size) body.append(el("p", { className: "hint", textContent: "No privileges granted directly." }));
      fill();
      syncButtons();
    };
    add.onclick = () => {
      const o = data.objects[Number(picker.value)];
      if (!picker.value || !o) return;
      rows.set(objKey(o), { on: o, privs: new Map() });
      render();
    };
    render();
    const scope = my ? "" : " Schemas and tables are those of the database this workspace is on.";
    return el("section", {},
      el("h4", { textContent: "Privileges" }),
      el("p", { className: "hint", textContent: `Privileges granted to this account directly; ones it gets from roles or PUBLIC don't show.${scope}` }),
      body, el("div", { className: "privilege-add" }, picker, add));
  }

  function privilegeRow(key: string, row: { on: Obj; privs: Map<string, boolean> }, rerender: () => void) {
    const offered = [...new Set([...(PRIVILEGES[engine][row.on.kind] ?? []), ...row.privs.keys()])];
    const option = el("input", { type: "checkbox" });
    const syncOption = () => {
      const values = [...row.privs.values()];
      option.checked = values.length > 0 && values.every(Boolean);
      option.indeterminate = values.some(Boolean) && !option.checked;
      option.disabled = !values.length;
    };
    option.onchange = () => {
      for (const p of row.privs.keys()) row.privs.set(p, option.checked);
      syncOption();
      syncButtons();
    };
    const boxes = offered.map((p) => {
      const c = el("input", { type: "checkbox", checked: row.privs.has(p) });
      c.onchange = () => {
        if (c.checked) row.privs.set(p, option.checked && !option.indeterminate);
        else row.privs.delete(p);
        syncOption();
        syncButtons();
      };
      return el("label", { className: "check" }, c, ` ${privLabel(p)}`);
    });
    const all = el("button", { type: "button", className: "icon", textContent: "All" });
    all.onclick = () => (offered.forEach((p) => row.privs.has(p) || row.privs.set(p, option.checked)), rerender());
    const remove = el("button", { type: "button", className: "icon", textContent: "Remove", title: "Revoke everything on it" });
    remove.onclick = () => (rows.delete(key), rerender());
    syncOption();
    return el("div", { className: "privilege-row" },
      el("div", { className: "privilege-head" },
        el("span", { className: "privilege-kind", textContent: kindLabel[row.on.kind] }),
        el("strong", { textContent: objLabel(row.on, my) }),
        el("span", { className: "spacer" }),
        el("label", { className: "check", title: my ? "MySQL keeps the grant option per object" : "" }, option, " With grant option"),
        all, remove),
      el("div", { className: "privilege-boxes" }, ...boxes));
  }

  function syncButtons() {
    save.disabled = !dirty();
    revert.disabled = !draft || !before || !dirty();
    drop.disabled = !before;
  }

  const editArgs = (after: Account | null) => ({ name: conn.name, edit: { [engine]: { before, after } }, password: password.value || null });

  /** Previews the edit's SQL and runs it once confirmed; true when it ran. */
  async function apply(after: Account | null, title: string, okLabel: string, danger: boolean) {
    try {
      const statements = await call<string[]>("user_sql", editArgs(after));
      if (!statements.length) return setStatus("Nothing to change"), false;
      if (!(await preview(title, statements, okLabel, danger, my))) return false;
      await guarded(conn.name, (a) => call("apply_user_edit", { ...editArgs(after), confirmed: true, unlock: a.unlock }));
      return true;
    } catch (e) {
      if (e !== CANCELLED) setStatus(String(e), "error");
      return false;
    }
  }

  save.onclick = async () => {
    const after = current();
    if (!after) return;
    const creating = !before;
    if (await apply(after, creating ? `Create ${accountLabel(after)}` : `Change ${accountLabel(before!)}`, creating ? "Create" : "Apply", false)) {
      password.value = "";
      await load(accountLabel(after));
      setStatus(creating ? `Created ${accountLabel(after)}` : "Saved", "done");
    }
  };
  drop.onclick = async () => {
    if (!before) return;
    const gone = accountLabel(before);
    if (await apply(null, `Drop ${gone}`, "Drop", true)) await load(), setStatus(`Dropped ${gone}`, "done");
  };
  revert.onclick = () => before && edit(clone(before));
  newButton.onclick = async () => (await leave()) && newAccount();
  close.onclick = () => d.close();
  d.addEventListener("cancel", async (e) => {
    if (!dirty()) return;
    e.preventDefault(); // Esc with unsaved edits asks first
    if (await leave()) d.close();
  });

  d.showModal();
  clearEditor();
  load();
}

/** The statements about to run; true when the user runs them. */
function preview(title: string, statements: string[], okLabel: string, danger: boolean, my: boolean): Promise<boolean> {
  const sql = statements.map((s) => `${s};`).join("\n\n");
  const cancel = el("button", { type: "button", textContent: "Cancel" });
  const copy = el("button", { type: "button", textContent: "Copy" });
  const ok = el("button", { type: "button", className: danger ? "danger" : "primary", textContent: okLabel });
  const d = el("dialog", { className: "structure-dialog wide" },
    el("h3", { textContent: title }), el("pre", { className: "sql-preview", textContent: sql }),
    el("p", { className: "hint", textContent: statements.length < 2 ? "" : my
      ? "MySQL commits each statement as it runs; if one fails, the ones before it stay applied."
      : "Runs in one transaction." }),
    el("footer", {}, el("span", { className: "spacer" }), cancel, copy, ok));
  document.body.append(d);
  d.showModal();
  cancel.focus(); // Enter never runs by accident
  return new Promise((resolve) => {
    let ran = false;
    copy.onclick = () => navigator.clipboard.writeText(sql).catch(() => {});
    cancel.onclick = () => d.close();
    ok.onclick = () => ((ran = true), d.close());
    d.onclose = () => (d.remove(), resolve(ran));
  });
}
