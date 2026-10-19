import { test } from "node:test";
import assert from "node:assert/strict";
import { TLS_DEFAULT, addressOf, dumps, engineId, hasProcesses, hostOf, managesUsers, networkUrl, sshTunnel, tlsSettings, type Saved } from "./engines.ts";

const blank = { host: "", port: "", user: "", database: "" };

test("networkUrl fills blank fields with the engine's defaults", () => {
  assert.equal(networkUrl("postgres", blank), "postgres://postgres@127.0.0.1:5432");
  assert.equal(networkUrl("mariadb", { ...blank, database: "shop" }), "mysql://root@127.0.0.1:3306/shop");
});

test("networkUrl encodes the user and database", () => {
  assert.equal(networkUrl("mysql", { host: "db", port: "3307", user: "a@b", database: "my db" }), "mysql://a%40b@db:3307/my%20db");
});

test("addressOf leaves default fields blank, so networkUrl gives the same URL back", () => {
  const defaults = { host: "127.0.0.1", port: "5432", user: "postgres", database: "" };
  const url = "postgres://a%40b@db.example.com:5432/my%20db";
  const a = addressOf(url, defaults);
  assert.deepEqual(a, { host: "db.example.com", port: "", user: "a@b", database: "my db" });
  assert.equal(networkUrl("postgres", a), url);
});

test("sshTunnel needs a host and a port from 1 to 65535", () => {
  assert.throws(() => sshTunnel({ host: "", port: "", user: "", key: "" }), /SSH host/);
  assert.throws(() => sshTunnel({ host: "h", port: "70000", user: "", key: "" }), /SSH port/);
  assert.throws(() => sshTunnel({ host: "h", port: "2.5", user: "", key: "" }), /SSH port/);
  assert.deepEqual(sshTunnel({ host: "h", port: "", user: "u", key: "" }), { host: "h", port: null, user: "u", key: "" });
  assert.equal(sshTunnel({ host: "h", port: "2222", user: "", key: "" }).port, 2222);
});

test("tlsSettings leaves out the defaults and the files of a disabled mode", () => {
  assert.equal(tlsSettings({ ...TLS_DEFAULT }), undefined);
  assert.deepEqual(tlsSettings({ mode: "disable", ca: "ca.pem", cert: "", key: "" }), { mode: "disable", ca: "", cert: "", key: "" });
  assert.deepEqual(tlsSettings({ mode: "require", ca: "", cert: "", key: "" }), { mode: "require", ca: "", cert: "", key: "" });
});

test("tlsSettings refuses a client certificate without its key", () => {
  assert.throws(() => tlsSettings({ mode: "verify-full", ca: "", cert: "c.pem", key: "" }), /both the certificate file and its key file/);
});

test("hostOf shows the host and database, the tunnel, or a SQLite path as is", () => {
  const c: Saved = { name: "n", kind: "postgres", url: "postgres://u@db:5432/shop", tag: "", safety: "silent" };
  assert.equal(hostOf(c), "db/shop");
  assert.equal(hostOf({ ...c, ssh: { host: "jump", port: null, user: "", key: "" } }), "db/shop via jump");
  assert.equal(hostOf({ ...c, kind: "sqlite", url: "/tmp/a.db" }), "/tmp/a.db");
});

test("networkUrl leaves out a Redis user that isn't set and defaults to database 0", () => {
  assert.equal(networkUrl("redis", blank), "redis://127.0.0.1:6379/0");
  assert.equal(networkUrl("redis", { ...blank, user: "app", database: "3" }), "redis://app@127.0.0.1:6379/3");
});

test("networkUrl refuses a Redis database that isn't a number", () => {
  assert.throws(() => networkUrl("redis", { ...blank, database: "cache" }), /Redis database is a number/);
});

test("networkUrl takes CockroachDB's defaults and writes a Postgres URL", () => {
  assert.equal(networkUrl("cockroach", blank), "postgres://root@127.0.0.1:26257");
});

test("networkUrl builds each engine's own URL shape", () => {
  assert.equal(networkUrl("mssql", { ...blank, host: "db\\SQLEXPRESS", port: "1444" }), "mssql://sa@db:1444/?instance=SQLEXPRESS");
  assert.equal(networkUrl("mssql", { ...blank, host: "db\\SQLEXPRESS" }), "mssql://sa@db/?instance=SQLEXPRESS");
  assert.equal(networkUrl("d1", { ...blank, user: "acct1", database: "db-1" }), "d1://acct1/db-1");
  assert.equal(networkUrl("dynamodb", { ...blank, user: "AKIA" }), "dynamodb://AKIA@dynamodb.us-east-1.amazonaws.com?region=us-east-1");
  assert.equal(networkUrl("bigquery", { ...blank, database: "proj" }), "bigquery://bigquery.googleapis.com/proj");
  assert.equal(networkUrl("libsql", { ...blank, host: "my-db.turso.io" }), "libsql://my-db.turso.io");
  assert.equal(networkUrl("clickhouse", blank), "clickhouse://default@127.0.0.1:8123");
});

test("networkUrl refuses what an engine can't connect without", () => {
  assert.throws(() => networkUrl("d1", blank), /account ID/);
  assert.throws(() => networkUrl("dynamodb", blank), /access key ID/);
  assert.throws(() => networkUrl("bigquery", blank), /project ID/);
  assert.throws(() => networkUrl("mssql", { ...blank, host: "db\\" }), /named instance/);
});

test("networkUrl gives MongoDB host lists and SRV names as typed, and one host its port", () => {
  assert.equal(networkUrl("mongodb", { ...blank, host: "h1" }), "mongodb://h1:27017/");
  assert.equal(networkUrl("mongodb", { ...blank, host: "h1:27017,h2:27017", user: "u", database: "app" }, { query: "?replicaSet=rs" }), "mongodb://u@h1:27017,h2:27017/app?replicaSet=rs");
  assert.equal(networkUrl("mongodb", { ...blank, host: "cluster.example.net" }, { srv: true }), "mongodb+srv://cluster.example.net/");
});

test("addressOf reads back what networkUrl wrote, for the engines whose URL isn't the usual shape", () => {
  const mongo = addressOf("mongodb://u@h1:27017,h2:27017/app?replicaSet=rs", { host: "127.0.0.1", port: "27017", user: "", database: "" }, "mongodb");
  assert.deepEqual(mongo, { host: "h1:27017,h2:27017", port: "", user: "u", database: "app", srv: false, query: "replicaSet=rs" });
  const none = { host: "", port: "", user: "", database: "" };
  assert.equal(addressOf("mssql://sa@db/?instance=SQLEXPRESS", none, "mssql").host, "db\\SQLEXPRESS");
  assert.equal(addressOf("d1://acct1/db-1", none, "d1").user, "acct1");
  assert.equal(addressOf("dynamodb://AKIA@h?region=eu-west-1", none, "dynamodb").database, "eu-west-1");
});

test("hostOf shows a DuckDB path as is", () => {
  assert.equal(hostOf({ name: "n", kind: "duckdb", url: "/tmp/a.duckdb", tag: "", safety: "silent" }), "/tmp/a.duckdb");
});

test("engineId is the flavor when there is one, and the capability checks follow the engine", () => {
  const base: Saved = { name: "n", kind: "postgres", url: "postgres://h", tag: "", safety: "silent" };
  assert.equal(engineId(base), "postgres");
  assert.equal(engineId({ ...base, flavor: "cockroach" }), "cockroach");
  assert.ok(dumps(base) && managesUsers(base) && hasProcesses(base));
  assert.ok(!dumps({ ...base, flavor: "cockroach" }) && !managesUsers({ ...base, flavor: "cockroach" }));
  assert.ok(!dumps({ ...base, kind: "mongodb" }) && !hasProcesses({ ...base, kind: "mongodb" }));
  assert.ok(!managesUsers({ ...base, kind: "duckdb" }) && !hasProcesses({ ...base, kind: "duckdb" }));
});
