// The engines a connection can use, a saved connection's shape, and the parts of the connection form that are plain
// logic: its URL, SSH tunnel and TLS settings. connform.ts draws the form.

import type { Safety } from "./safety";
import { el } from "./dom.ts"; // with the extension, for engines.test.ts under node

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
};
type Address = { host: string; port: string; user: string; database: string };
export type Engine = {
  kind: string;
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
  { kind: "postgres", label: "PostgreSQL", mono: "Pg", color: "var(--blue)",
    defaults: { host: "127.0.0.1", port: "5432", user: "postgres", database: "" } },
  { kind: "mysql", label: "MySQL", mono: "Ms", color: "var(--orange)",
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "" } },
  { kind: "mariadb", label: "MariaDB", mono: "Ma", color: "var(--pink)",
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "" } },
  { kind: "sqlite", label: "SQLite", mono: "Sl", color: "var(--yellow)" },
  { kind: "redis", label: "Redis", mono: "Rd", color: "var(--danger)",
    defaults: { host: "127.0.0.1", port: "6379", user: "", database: "0" } },
];
export const engine = (kind: string) => ENGINES.find((e) => e.kind === kind) ?? ENGINES[0];

export function avatar(kind: string) {
  const e = engine(kind);
  const a = el("span", { className: "avatar", textContent: e.mono, title: e.label });
  a.style.background = e.color;
  return a;
}

export function hostOf(c: Saved) {
  if (c.kind === "sqlite") return c.url;
  const via = c.ssh ? ` via ${c.ssh.host}` : "";
  try {
    const u = new URL(c.url);
    return u.hostname + (u.pathname.length > 1 ? decodeURIComponent(u.pathname) : "") + via;
  } catch {
    return c.url + via;
  }
}

/** A network connection's URL from the form's (trimmed) fields; a blank one takes the engine's default. Throws what's
 *  wrong with them. */
export function networkUrl(kind: string, a: Address) {
  const d = engine(kind).defaults!;
  const host = a.host || d.host;
  const port = a.port || d.port;
  const user = a.user || d.user;
  const db = a.database || d.database;
  const scheme = kind === "postgres" ? "postgres" : kind === "redis" ? "redis" : "mysql";
  if (kind === "redis" && !/^\d+$/.test(db || "0")) throw "A Redis database is a number, such as 0.";
  return `${scheme}://${user ? encodeURIComponent(user) + "@" : ""}${host}:${port}${db ? "/" + encodeURIComponent(db) : ""}`;
}

/** The form's fields for a saved URL: only those that differ from the default, so placeholders keep meaning
 *  "default". */
export function addressOf(url: string, d: Address): Address {
  const u = new URL(url);
  const set = (k: keyof Address, v: string) => (v && v !== d[k] ? v : "");
  return {
    host: set("host", u.hostname),
    port: set("port", u.port),
    user: set("user", decodeURIComponent(u.username)),
    database: set("database", decodeURIComponent(u.pathname.slice(1))),
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
