import { test } from "node:test";
import assert from "node:assert/strict";
import { cell, columnsOf, filterFor, itemText, literal, typedLiteral } from "./dynamo-text.ts";

test("literal writes each typed value in PartiQL's syntax", () => {
  assert.equal(literal({ S: "it's" }), "'it''s'");
  assert.equal(literal({ N: "-1.5" }), "-1.5");
  assert.equal(literal({ BOOL: false }), "false");
  assert.equal(literal({ NULL: true }), "null");
  assert.equal(literal({ L: [{ N: "1" }, { S: "a" }] }), "[1, 'a']");
  assert.equal(literal({ M: { city: { S: "Oslo" } } }), "{'city': 'Oslo'}");
  assert.equal(literal({ SS: ["x", "y"] }), "<<'x', 'y'>>");
  assert.equal(literal({ NS: ["1", "2"] }), "<<1, 2>>");
  assert.equal(literal({ B: "qrvM" }), "'<3 bytes>'");
});

test("cell shows text and numbers plainly, NULL as null and a missing attribute empty", () => {
  assert.equal(cell({ S: "Rex" }), "Rex");
  assert.equal(cell({ N: "3" }), "3");
  assert.equal(cell({ NULL: true }), null);
  assert.equal(cell(undefined), "");
  assert.equal(cell({ SS: ["x"] }), "<<'x'>>");
});

test("columnsOf puts the key first", () => {
  assert.deepEqual(columnsOf([{ name: { S: "a" }, n: { N: "1" }, pk: { S: "p" } }, { other: { S: "o" } }], ["pk", "n"]), ["pk", "n", "name", "other"]);
});

test("typedLiteral keeps text as text and reads typed values as written", () => {
  assert.equal(typedLiteral({ S: "Rex" }, "42"), "'42'");
  assert.equal(typedLiteral({ N: "3" }, "4"), "4");
  assert.equal(typedLiteral({ N: "3" }, "four"), "'four'");
  assert.equal(typedLiteral(undefined, "<<'a'>>"), "<<'a'>>");
  assert.equal(typedLiteral(undefined, "Ada's"), "'Ada''s'");
});

test("itemText lays an item out an attribute a line, and filterFor quotes the name", () => {
  assert.equal(itemText({ pk: { S: "a" }, n: { N: "1" } }), "{\n  'pk': 'a',\n  'n': 1\n}");
  assert.equal(filterFor('my"col', { S: "x" }), "\"my\"\"col\" = 'x'");
});
