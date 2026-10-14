// Run with `node --test 'src/**/*.test.ts'`. Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { fillParams, findParams, sourceTable, splitStatements, statementAt } from "./statements.ts";

const texts = (sql: string, d: "postgres" | "mysql" | "sqlite" = "postgres") => splitStatements(sql, d).map((s) => s.text);

test("splits at semicolons and trims", () => {
  assert.deepEqual(texts("select 1;\n  select 2 ;\n"), ["select 1", "select 2"]);
});

test("ignores semicolons inside strings, quoted names and comments", () => {
  const sql = `select ';' as "a;b"; -- not; here\nselect 2 /* nor; here */; select 'it''s;'`;
  assert.deepEqual(texts(sql), [`select ';' as "a;b"`, "-- not; here\nselect 2 /* nor; here */", "select 'it''s;'"]);
});

test("handles postgres dollar quotes but not $1 parameters", () => {
  const sql = "create function f() returns int as $body$ begin; return 1; end $body$ language plpgsql; select $1";
  assert.deepEqual(texts(sql), ["create function f() returns int as $body$ begin; return 1; end $body$ language plpgsql", "select $1"]);
});

test("knows mysql backslash escapes and # comments", () => {
  assert.deepEqual(texts("select 'a\\';b'; # c;d\nselect 2", "mysql"), ["select 'a\\';b'", "# c;d\nselect 2"]);
});

test("drops statements that are only comments", () => {
  assert.deepEqual(texts("select 1; -- the end\n"), ["select 1"]);
});

test("statementAt finds the statement under the cursor, or the one just before", () => {
  const sql = "select 1;\n\nselect 2;";
  assert.equal(statementAt(sql, 3, "sqlite")?.text, "select 1");
  assert.equal(statementAt(sql, 9, "sqlite")?.text, "select 1", "right after the semicolon");
  assert.equal(statementAt(sql, 10, "sqlite")?.text, "select 1", "on the blank line below");
  assert.equal(statementAt(sql, sql.length, "sqlite")?.text, "select 2");
});

const names = (sql: string, f: Parameters<typeof findParams>[2] = "colon") => findParams(sql, "postgres", f).map((p) => p.name);

test("colon params skip ::casts, strings and comments", () => {
  assert.deepEqual(names("select :id::int, ':no', \"x:no\" -- :no\nwhere a = :name and b = :u.id"), ["id", "name", "u.id"]);
});

test("each format finds its own placeholders", () => {
  assert.deepEqual(names("where a = $who and b = $1", "dollar"), ["who"]);
  assert.deepEqual(names("where a = ${who}", "dollarBraces"), ["who"]);
  assert.deepEqual(names("where a = {{who}}", "doubleBraces"), ["who"]);
  assert.deepEqual(names("where a = %who%", "percent"), ["who"]);
  assert.deepEqual(names("where a = ? and b = '?' and c = ?", "question"), ["1", "2"]);
});

test("fillParams puts values in as typed, repeated names included", () => {
  const sql = "select * from t where a = :x or b = :x and c = :name";
  const filled = fillParams(sql, findParams(sql, "postgres", "colon"), { x: "1", name: "'Ada'" });
  assert.equal(filled, "select * from t where a = 1 or b = 1 and c = 'Ada'");
});

const tables = ["public.people", "public.pets", "app.orders"];

test("sourceTable finds the one table a plain select reads", () => {
  assert.equal(sourceTable("select * from people where id > 2 order by name limit 10;", tables), "public.people");
  assert.equal(sourceTable('SELECT id, name FROM "public"."pets" p', tables), "public.pets");
  assert.equal(sourceTable("select * from orders", tables), "app.orders");
  assert.equal(sourceTable("-- note\nselect * from People", tables), "public.people");
});

test("sourceTable refuses what doesn't map onto one table's rows", () => {
  assert.equal(sourceTable("select * from people join pets on true", tables), null);
  assert.equal(sourceTable("select * from people, pets", tables), null);
  assert.equal(sourceTable("select count(*) from people group by name", tables), null);
  assert.equal(sourceTable("select distinct name from people", tables), null);
  assert.equal(sourceTable("select * from people union select * from pets", tables), null);
  assert.equal(sourceTable("select (select 1) from people", tables), null);
  assert.equal(sourceTable("select * from unknown", tables), null);
  assert.equal(sourceTable("update people set a = 1", tables), null);
});
