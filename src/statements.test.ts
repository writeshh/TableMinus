// Run with `node --test 'src/**/*.test.ts'`. Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { splitStatements, statementAt } from "./statements.ts";

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
