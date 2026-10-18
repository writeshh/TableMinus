import { test } from "node:test";
import assert from "node:assert/strict";
import { cell, columnsOf, countScript, documentText, expression, pageScript, shellText, typedExpression } from "./mongo-text.ts";

test("shellText writes Extended JSON wrappers as mongosh does", () => {
  assert.equal(shellText({ $oid: "64b7f0c2a1b2c3d4e5f60718" }), 'ObjectId("64b7f0c2a1b2c3d4e5f60718")');
  assert.equal(shellText({ $date: "2024-01-02T00:00:00Z" }), 'ISODate("2024-01-02T00:00:00Z")');
  assert.equal(shellText({ $date: { $numberLong: "-1000" } }), 'ISODate("1969-12-31T23:59:59.000Z")');
  assert.equal(shellText({ $numberDecimal: "1.10" }), 'NumberDecimal("1.10")');
  assert.equal(shellText({ $regularExpression: { pattern: "^a", options: "i" } }), "/^a/i");
  assert.equal(shellText({ $binary: { base64: "AAECAwQFBgcICQoLDA0ODw==", subType: "04" } }), 'UUID("00010203-0405-0607-0809-0a0b0c0d0e0f")');
  assert.equal(shellText({ $binary: { base64: "qw==", subType: "00" } }), 'BinData(0, "qw==")');
});

test("shellText writes documents with plain keys where it can, and lays them out for editing", () => {
  assert.equal(shellText({ a: 1, "b c": "x", d: [true, null], e: {} }), '{ a: 1, "b c": "x", d: [true, null], e: {} }');
  assert.equal(documentText({ _id: { $oid: "64b7f0c2a1b2c3d4e5f60718" }, tags: ["a"] }), '{\n  _id: ObjectId("64b7f0c2a1b2c3d4e5f60718"),\n  tags: [\n    "a"\n  ]\n}');
});

test("cell shows text plainly, keeps null apart from a missing field", () => {
  assert.equal(cell("Rex"), "Rex");
  assert.equal(cell(4.5), "4.5");
  assert.equal(cell(null), null);
  assert.equal(cell(undefined), "");
  assert.equal(cell({ $oid: "64b7f0c2a1b2c3d4e5f60718" }), 'ObjectId("64b7f0c2a1b2c3d4e5f60718")');
});

test("columnsOf puts _id first and keeps first-seen order", () => {
  assert.deepEqual(columnsOf([{ name: "a", _id: 1 }, { age: 2, name: "b" }]), ["_id", "name", "age"]);
  assert.deepEqual(columnsOf([{ n: 1 }]), ["n"], "no _id when no document has one (a projection)");
});

test("typedExpression keeps text as text and reads typed values as written", () => {
  assert.equal(typedExpression("Rex", "42"), '"42"', "a text field stays text");
  assert.equal(typedExpression(3, "42"), "42");
  assert.equal(typedExpression(3, "forty"), '"forty"');
  assert.equal(typedExpression(null, "ObjectId(\"64b7f0c2a1b2c3d4e5f60718\")"), 'ObjectId("64b7f0c2a1b2c3d4e5f60718")');
  assert.equal(typedExpression(undefined, "{ a: 1 }"), "{ a: 1 }");
  assert.equal(typedExpression(true, "false"), "false");
  assert.equal(expression({ $oid: "x" }), '{"$oid":"x"}');
});

test("pageScript and countScript quote the collection and leave the filter as typed", () => {
  assert.equal(pageScript('a"b', " { n: { $gt: 1 } } ", "{ n: -1 }", 100, 50), 'db.getCollection("a\\"b").find({ n: { $gt: 1 } }).sort({ n: -1 }).skip(100).limit(50)');
  assert.equal(pageScript("t", "", "", 0, 10), 'db.getCollection("t").find({}).skip(0).limit(10)');
  assert.equal(countScript("t", ""), 'db.getCollection("t").countDocuments({})');
});
