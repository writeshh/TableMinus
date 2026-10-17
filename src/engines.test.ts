import { test } from "node:test";
import assert from "node:assert/strict";
import { TLS_DEFAULT, addressOf, hostOf, networkUrl, sshTunnel, tlsSettings, type Saved } from "./engines.ts";

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
