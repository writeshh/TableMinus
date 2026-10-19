// The engines a connection can use, a saved connection's shape, and the parts of the connection form that are plain
// logic: its URL, SSH tunnel and TLS settings. connform.ts draws the form.

import type { Safety } from "../shell/safety";
import { el } from "../dom.ts"; // with the extension, for engines.test.ts under node

export type Ssh = { host: string; port: number | null; user: string; key: string };
export type Tls = { mode: string; ca: string; cert: string; key: string };
export type Options = {
  startup_sql: string; startup_script: string;
  direct_tls: boolean; no_channel_binding: boolean; system_schemas: boolean;
  socket: string; sni: string; cleartext_auth: boolean;
};
export const OPTIONS_DEFAULT: Options = {
  startup_sql: "", startup_script: "", direct_tls: false, no_channel_binding: false, system_schemas: false,
  socket: "", sni: "", cleartext_auth: false,
};
export type Saved = {
  name: string; kind: string; url: string; tag: string; safety: Safety; ssh?: Ssh; tls?: Tls; options?: Options;
  password_mode?: "keychain" | "ask" | "no_password"; color?: string | null; group?: string;
  /** A Postgres-compatible server it is shown as; it connects as `kind`. */
  flavor?: "cockroach";
  /** Found by discovery on a server that refused the default sign-in; cleared by the backend on the first connect. */
  needs_credentials?: boolean;
};
export type Address = { host: string; port: string; user: string; database: string };
/** What only some engines' forms have: MongoDB's SRV switch and its connection string options. */
export type Extra = { srv?: boolean; query?: string };
export type Engine = {
  /** The picker entry: the kind, or the flavor for a Postgres-compatible server. */
  id: string;
  kind: string;
  flavor?: "cockroach";
  label: string;
  mono: string;
  color: string;
  // Every field is optional: a blank input uses the default, shown as its placeholder.
  defaults?: Address;
};

// libpq's sslmode values; MySQL's ssl-mode means the same. Prefer and Require encrypt without checking the
// certificate, as libpq does.
export const TLS_MODES: [string, string][] = [
  ["disable", "Disable"], ["prefer", "Prefer"], ["require", "Require"],
  ["verify-ca", "Verify CA"], ["verify-full", "Verify CA and host name"],
];
export const TLS_DEFAULT: Tls = { mode: "prefer", ca: "", cert: "", key: "" };
export const ENGINES: Engine[] = [
  { id: "postgres", kind: "postgres", label: "PostgreSQL", mono: "Pg", color: "var(--blue)",
    defaults: { host: "127.0.0.1", port: "5432", user: "postgres", database: "" } },
  { id: "cockroach", kind: "postgres", flavor: "cockroach", label: "CockroachDB", mono: "Cr", color: "var(--ok)",
    defaults: { host: "127.0.0.1", port: "26257", user: "root", database: "" } },
  { id: "mysql", kind: "mysql", label: "MySQL", mono: "Ms", color: "var(--orange)",
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "" } },
  { id: "mariadb", kind: "mariadb", label: "MariaDB", mono: "Ma", color: "var(--pink)",
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "" } },
  { id: "sqlite", kind: "sqlite", label: "SQLite", mono: "Sl", color: "var(--yellow)" },
  { id: "duckdb", kind: "duckdb", label: "DuckDB", mono: "Dk", color: "var(--accent)" },
  { id: "mssql", kind: "mssql", label: "SQL Server", mono: "Ss", color: "var(--danger)",
    defaults: { host: "127.0.0.1", port: "1433", user: "sa", database: "" } },
  { id: "clickhouse", kind: "clickhouse", label: "ClickHouse", mono: "Ch", color: "var(--yellow)",
    defaults: { host: "127.0.0.1", port: "8123", user: "default", database: "" } },
  { id: "oracle", kind: "oracle", label: "Oracle", mono: "Or", color: "var(--danger)",
    defaults: { host: "127.0.0.1", port: "1521", user: "system", database: "FREEPDB1" } },
  { id: "bigquery", kind: "bigquery", label: "BigQuery", mono: "Bq", color: "var(--blue)",
    defaults: { host: "bigquery.googleapis.com", port: "", user: "", database: "" } },
  { id: "libsql", kind: "libsql", label: "libSQL / Turso", mono: "Ls", color: "var(--ok)",
    defaults: { host: "127.0.0.1", port: "", user: "", database: "" } },
  { id: "d1", kind: "d1", label: "Cloudflare D1", mono: "D1", color: "var(--orange)",
    defaults: { host: "", port: "", user: "", database: "" } },
  { id: "mongodb", kind: "mongodb", label: "MongoDB", mono: "Mg", color: "var(--ok)",
    defaults: { host: "127.0.0.1", port: "27017", user: "", database: "" } },
  { id: "dynamodb", kind: "dynamodb", label: "Amazon DynamoDB", mono: "Dy", color: "var(--blue)",
    defaults: { host: "dynamodb.us-east-1.amazonaws.com", port: "", user: "", database: "us-east-1" } },
  { id: "elasticsearch", kind: "elasticsearch", label: "Elasticsearch", mono: "Es", color: "var(--yellow)",
    defaults: { host: "127.0.0.1", port: "9200", user: "", database: "" } },
  { id: "cassandra", kind: "cassandra", label: "Cassandra", mono: "Ca", color: "var(--blue)",
    defaults: { host: "127.0.0.1", port: "9042", user: "", database: "" } },
  { id: "redis", kind: "redis", label: "Redis", mono: "Rd", color: "var(--danger)",
    defaults: { host: "127.0.0.1", port: "6379", user: "", database: "0" } },
];
export const engine = (id: string) => ENGINES.find((e) => e.id === id) ?? ENGINES[0];
/** The picker entry a saved connection belongs to. */
export const engineId = (c: { kind: string; flavor?: string }) => c.flavor ?? c.kind;
/** SQLite and DuckDB are files: no host, port, user or tunnel. */
export const isFile = (kind: string) => kind === "sqlite" || kind === "duckdb";

// CockroachDB has no pg_dump or pg_restore, and its roles take other attributes than Postgres's.
// SQL Server and ClickHouse back up with their own statements.
// libSQL and D1 have their own (turso db shell .dump, wrangler d1 export).
export const dumps = (c: Saved) =>
  c.flavor !== "cockroach" && !["duckdb", "mssql", "clickhouse", "oracle", "bigquery", "libsql", "d1", "mongodb", "dynamodb", "elasticsearch", "cassandra"].includes(c.kind);
export const managesUsers = (c: Saved) =>
  !isFile(c.kind) && c.flavor !== "cockroach" && !["mssql", "clickhouse", "oracle", "bigquery", "libsql", "d1", "mongodb", "dynamodb", "elasticsearch", "cassandra"].includes(c.kind);
/** Engines with sessions to list and stop. */
export const hasProcesses = (c: Saved) => !isFile(c.kind) && !["libsql", "d1", "mongodb", "dynamodb", "elasticsearch", "cassandra"].includes(c.kind);

/** How the connection form differs for engines whose fields mean something else (`data-f` names the field). */
export const FORM_SHAPES: Record<string, { hide: string[]; label?: Record<string, string>; placeholder?: Record<string, string> }> = {
  libsql: {
    hide: ["user", "database"],
    label: { password: "Auth token" },
    placeholder: { host: "127.0.0.1, or my-db-org.turso.io", port: "443, or 8080 for a local server without SSL" },
  },
  dynamodb: {
    hide: [],
    label: { user: "Access key ID", password: "Secret access key", database: "Region" },
    placeholder: {
      host: "dynamodb.us-east-1.amazonaws.com, or localhost for DynamoDB Local", port: "443, or 8000 for DynamoDB Local", database: "us-east-1",
    },
  },
  mssql: {
    hide: [],
    placeholder: { host: "127.0.0.1, or host\\SQLEXPRESS for a named instance", port: "1433; blank finds a named instance's port" },
  },
  bigquery: {
    hide: ["user"],
    label: { password: "Service account key", database: "Project ID" },
    placeholder: {
      host: "bigquery.googleapis.com, or 127.0.0.1 for the emulator", port: "443, or 9050 for the emulator",
      password: "The key's JSON, pasted whole (none for the emulator)", database: "my-project",
    },
  },
  oracle: {
    hide: [],
    label: { database: "Service name" },
    placeholder: { database: "FREEPDB1, ORCLPDB1 or XEPDB1" },
  },
  cassandra: {
    hide: [],
    label: { database: "Keyspace" },
    placeholder: { user: "Optional", database: "Optional, choose one after connecting" },
  },
  elasticsearch: {
    hide: ["database"],
    label: { password: "Password or API key" },
    placeholder: { user: "Optional; leave empty to send the password as an API key" },
  },
  d1: {
    hide: ["host", "port", "ssl", "via", "password_mode"],
    label: { user: "Account ID", database: "Database ID", password: "API token" },
    placeholder: { user: "From the Cloudflare dashboard", database: "From the D1 database's page" },
  },
};

export function avatar(id: string) {
  const e = engine(id);
  const a = el("span", { className: "avatar", textContent: e.mono, title: e.label });
  a.style.background = e.color;
  return a;
}

export function hostOf(c: Saved) {
  if (isFile(c.kind)) return c.url;
  const via = c.ssh ? ` via ${c.ssh.host}` : "";
  try {
    const u = new URL(c.url);
    return u.hostname + (u.pathname.length > 1 ? decodeURIComponent(u.pathname) : "") + via;
  } catch {
    return c.url + via;
  }
}

/** A network connection's URL from the form's (trimmed) fields; a blank one takes the engine's default (`id` is the
 *  picker entry). Throws what's wrong with them. */
export function networkUrl(id: string, a: Address, extra: Extra = {}) {
  const e = engine(id);
  const kind = e.kind;
  const d = e.defaults!;
  const host = a.host || d.host;
  const port = a.port || d.port;
  const user = a.user || d.user;
  const db = a.database || d.database;
  const scheme = ["postgres", "redis", "mssql", "clickhouse", "oracle", "libsql", "elasticsearch", "cassandra"].includes(kind) ? kind : "mysql";
  const userPart = user ? encodeURIComponent(user) + "@" : "";
  if (kind === "redis" && !/^\d+$/.test(db || "0")) throw "A Redis database is a number, such as 0.";
  if (kind === "d1") {
    if (!/^[\w-]+$/.test(user) || !/^[\w-]+$/.test(db)) throw "Enter the Cloudflare account ID and the D1 database ID.";
    return `d1://${user}/${db}`;
  }
  if (kind === "mongodb") {
    const query = (extra.query ?? "").trim().replace(/^\?/, "");
    // A host list (h1:27017,h2:27017) or a seed list name goes as typed; one host takes the port.
    const hosts = extra.srv || /[,:]/.test(host) ? host : `${host}:${port}`;
    return `mongodb${extra.srv ? "+srv" : ""}://${userPart}${hosts}/${db ? encodeURIComponent(db) : ""}${query ? "?" + query : ""}`;
  }
  if (kind === "dynamodb") {
    if (!user) throw "Enter the access key ID.";
    return `dynamodb://${encodeURIComponent(user)}@${host}${port ? ":" + port : ""}?region=${encodeURIComponent(db || "us-east-1")}`;
  }
  if (kind === "mssql" && host.includes("\\")) {
    // host\instance: SQL Browser tells the instance's port, unless one is typed.
    const [server, instance] = host.split("\\", 2);
    if (!server || !instance) throw "Write a named instance as host\\INSTANCE.";
    return `mssql://${userPart}${server}${a.port ? ":" + a.port : ""}/${db ? encodeURIComponent(db) : ""}?instance=${encodeURIComponent(instance)}`;
  }
  if (kind === "bigquery") {
    if (!db) throw "Enter the Google Cloud project ID.";
    return `bigquery://${host}${port ? ":" + port : ""}/${encodeURIComponent(db)}`; // its first dataset is current
  }
  if (kind === "libsql") {
    if (!host) throw "Enter the server's host, such as my-db-org.turso.io.";
    return `libsql://${host}${port ? ":" + port : ""}`; // no port: 443 with SSL, 8080 without
  }
  return `${scheme}://${userPart}${host}:${port}${db ? "/" + encodeURIComponent(db) : ""}`;
}

/** The form's fields for a saved URL: only those that differ from the default, so placeholders keep meaning
 *  "default". `kind` says how the engine spreads its URL over the fields. */
export function addressOf(url: string, d: Address, kind = ""): Address & Extra {
  if (kind === "mongodb") {
    // Not a URL the browser reads when it lists several hosts (h1:27017,h2:27017).
    const m = /^mongodb(\+srv)?:\/\/(?:([^@/]*)@)?([^/?]*)\/?([^?]*)(?:\?(.*))?$/.exec(url);
    if (!m) return { host: "", port: "", user: "", database: "" };
    const [, srv, user, hosts, db, query] = m;
    const single = /^([^,:]+)(?::(\d+))?$/.exec(hosts);
    return {
      host: single ? single[1] : hosts, port: single?.[2] && single[2] !== "27017" ? single[2] : "",
      user: decodeURIComponent(user ?? ""), database: decodeURIComponent(db), srv: !!srv, query: query ?? "",
    };
  }
  const u = new URL(url);
  const pick = (k: keyof Address, v: string) => (v && v !== d[k] ? v : "");
  const instance = kind === "mssql" ? u.searchParams.get("instance") : null;
  return {
    host: instance ? `${u.hostname}\\${instance}` : pick("host", u.hostname),
    port: pick("port", u.port),
    user: pick("user", decodeURIComponent(kind === "d1" ? u.hostname : u.username)),
    database: pick("database", kind === "dynamodb" ? u.searchParams.get("region") ?? "" : decodeURIComponent(u.pathname.slice(1))),
  };
}

/** The SSH tunnel from the form's (trimmed) fields; throws what's wrong with them. */
export function sshTunnel(v: { host: string; port: string; user: string; key: string }): Ssh {
  if (!v.host) throw "Enter the SSH host, or choose Connect: Directly.";
  const port = v.port ? Number(v.port) : null;
  if (port !== null && !(Number.isInteger(port) && port > 0 && port < 65536)) throw "The SSH port must be a number from 1 to 65535.";
  return { host: v.host, port, user: v.user, key: v.key };
}

/** The TLS settings to save, or undefined when they're the defaults; throws what's wrong with them. */
export function tlsSettings(tls: Tls): Tls | undefined {
  if (tls.mode === "disable") tls = { ...tls, ca: "", cert: "", key: "" };
  if (!tls.cert !== !tls.key) throw "A client certificate needs both the certificate file and its key file.";
  const isDefault = (Object.keys(TLS_DEFAULT) as (keyof Tls)[]).every((k) => tls[k] === TLS_DEFAULT[k]);
  return isDefault ? undefined : tls;
}
