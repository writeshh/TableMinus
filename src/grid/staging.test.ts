import { test } from "node:test";
import assert from "node:assert/strict";
import { Staged, changesFor, columnKind, hexDump, nativeValue, parseTsv, pasteCells, sameJson } from "./staging.ts";
import { plain } from "./copy.ts";

test("Staged counts edits, added and deleted rows, and an edit back to the original unstages it", () => {
  const s = new Staged();
  s.change(() => s.set(0, 1, "x", 3, "a"));
  s.change(() => s.added.push(new Map()));
  s.change(() => s.remove([2], 3));
  assert.equal(s.count, 3);
  s.change(() => s.set(0, 1, "a", 3, "a"));
  assert.equal(s.count, 2);
});

test("Staged undo and redo step through changes, and a new change drops the redo", () => {
  const s = new Staged();
  s.change(() => s.set(0, 0, "1", 2, "0"));
  s.change(() => s.set(0, 0, "2", 2, "0"));
  assert.ok(s.undo());
  assert.equal(s.edits.get(0)?.get(0), "1");
  assert.ok(s.redo());
  assert.equal(s.edits.get(0)?.get(0), "2");
  s.undo();
  s.change(() => s.remove([1], 2));
  assert.equal(s.redo(), false);
  s.undo();
  s.undo();
  assert.equal(s.count, 0);
  assert.equal(s.undo(), false);
});

test("Staged remove drops added rows and toggles page rows back when all were deleted", () => {
  const s = new Staged();
  s.added.push(new Map([[0, "a"]]), new Map([[0, "b"]]));
  s.remove([1, 2], 2); // page row 1, and the first added row
  assert.deepEqual([...s.deleted], [1]);
  assert.deepEqual(s.added.map((r) => r.get(0)), ["b"]);
  s.remove([1], 2);
  assert.equal(s.deleted.size, 0);
});

test("parseTsv reads back what plain copies, quotes and NULL included", () => {
  const cells = { columns: ["a", "b"], rows: [["1", 'say "hi"\tthere'], [null, "two\nlines"]] };
  assert.deepEqual(parseTsv(plain(cells)), cells.rows);
});

test("parseTsv takes one value as is and ignores a final line break", () => {
  assert.deepEqual(parseTsv("hello\n"), [["hello"]]);
  assert.deepEqual(parseTsv("a\tb\r\nc\td\r\n"), [["a", "b"], ["c", "d"]]);
  assert.deepEqual(parseTsv('"unclosed'), [['"unclosed']]);
});

test("pasteCells fills a selection with one value and clips a block to the grid", () => {
  const block = { r0: 1, c0: 1, r1: 2, c1: 1 };
  assert.deepEqual(pasteCells([["x"]], block, 5, 3), [[1, 1, "x"], [2, 1, "x"]]);
  assert.deepEqual(pasteCells([["a", "b", "c"], ["d", "e", "f"]], block, 2, 3), [[1, 1, "a"], [1, 2, "b"]]);
});

test("columnKind reads each engine's type names", () => {
  assert.equal(columnKind("bytea", "postgres"), "binary");
  assert.equal(columnKind("mediumblob", "mysql"), "binary");
  assert.equal(columnKind("varbinary(16)", "mysql"), "binary");
  assert.equal(columnKind("tinyint(1)", "mysql"), "bool");
  assert.equal(columnKind("bool", "postgres"), "bool");
  assert.equal(columnKind("jsonb", "postgres"), "json");
  assert.equal(columnKind("datetime(3)", "mysql"), "datetime");
  assert.equal(columnKind("timestamptz", "postgres"), "text", "a native input would drop the time zone");
  assert.equal(columnKind("varchar(20)", "postgres"), "text");
});

test("nativeValue converts what a native input can show and refuses the rest", () => {
  assert.equal(nativeValue("datetime", "2026-01-02 03:04:05.678"), "2026-01-02T03:04:05.678");
  assert.equal(nativeValue("datetime", "2026-01-02 03:04:05.678901"), null);
  assert.equal(nativeValue("date", "infinity"), null);
  assert.equal(nativeValue("date", null), "");
});

test("sameJson ignores formatting only", () => {
  assert.ok(sameJson('{"a": [1, 2]}', '{\n  "a": [\n    1,\n    2\n  ]\n}'));
  assert.ok(!sameJson('{"a": 1}', '{"a": 2}'));
});

test("hexDump shows offsets, bytes and printable characters", () => {
  assert.equal(hexDump("48690a"), "00000000  48 69 0a" + " ".repeat(40) + "  Hi.");
});

test("changesFor skips edits to deleted rows and finds rows by key or identity", () => {
  const s = new Staged();
  s.set(0, 1, "new", 2, "old");
  s.set(1, 1, "gone anyway", 2, "b");
  s.deleted.add(1);
  s.added.push(new Map([[1, { file: "/tmp/x" }]]));
  const rows = [["1", "old"], ["2", "b"]];
  const key = { names: ["id"], idx: [0] };
  assert.deepEqual(changesFor(s, ["id", "v"], rows, key), {
    deletes: [{ key: [["id", "2"]] }],
    updates: [{ key: [["id", "1"]], set: [["v", "new"]] }],
    inserts: [[["v", { file: "/tmp/x" }]]],
  });
  assert.deepEqual(changesFor(s, ["id", "v"], rows, key, ["(0,1)", "(0,2)"]).deletes, [{ key: [["id", "2"]], id: "(0,2)" }]);
});

test("changesFor finds rows without key or identity by every value, and sends values to log when asked", () => {
  const s = new Staged();
  s.deleted.add(0);
  const rows = [["1", null]];
  assert.deepEqual(changesFor(s, ["n", "v"], rows, null).deletes, [{ key: [], original: [["n", "1"], ["v", null]] }]);
  const logged = changesFor(s, ["n", "v"], rows, { names: ["n"], idx: [0] }, undefined, true);
  assert.deepEqual(logged.deletes, [{ key: [["n", "1"]], original: [["n", "1"], ["v", null]] }]);
  assert.equal(logged.log_originals, true);
});
