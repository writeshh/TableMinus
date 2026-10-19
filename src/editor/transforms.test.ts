// Run with `node --test src/` (Node strips the types itself). Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { capitalize, convertIndent, replaceCells, transformText, trimTrailing } from "./transforms.ts";

test("capitalize uppercases each word's first letter and lowercases the rest", () => {
  assert.equal(capitalize("select FIRST_name from it's élan"), "Select First_name From It's Élan");
});

test("trimTrailing removes spaces and tabs at line ends only", () => {
  assert.equal(trimTrailing("a  \n\tb\t\n c"), "a\n\tb\n c");
});

test("convertIndent turns leading spaces into tabs and keeps a remainder", () => {
  assert.equal(convertIndent("    a\n      b\n  x  y", true, 4), "\ta\n\t  b\n  x  y");
});

test("convertIndent expands tabs to their column, not a fixed count", () => {
  assert.equal(convertIndent("\ta\n  \tb", false, 4), "    a\n    b");
});

test("transformText applies upper and lower case", () => {
  assert.equal(transformText("Ab", "upper", 2), "AB");
  assert.equal(transformText("Ab", "lower", 2), "ab");
});

test("replaceCells replaces every match in a cell, ignoring case, and keeps the rest of the text", () => {
  assert.deepEqual(replaceCells([["Foo foo FOO", "bar"]], "foo", "x", { matchCase: false, wholeCell: false }), [[0, 0, "x x x"]]);
});

test("replaceCells with match case leaves other casings alone", () => {
  assert.deepEqual(replaceCells([["Foo foo"]], "foo", "x", { matchCase: true, wholeCell: false }), [[0, 0, "Foo x"]]);
});

test("replaceCells with whole cell only changes cells equal to the text", () => {
  assert.deepEqual(replaceCells([["a"], ["ab"], ["A"]], "a", "z", { matchCase: false, wholeCell: true }), [[0, 0, "z"], [2, 0, "z"]]);
});

test("replaceCells skips NULL cells and cells that would not change", () => {
  assert.deepEqual(replaceCells([[null, "same"]], "same", "same", { matchCase: false, wholeCell: false }), []);
});

test("replaceCells does nothing for an empty search", () => {
  assert.deepEqual(replaceCells([["abc"]], "", "x", { matchCase: false, wholeCell: false }), []);
});

test("replaceCells treats the search and the replacement as plain text", () => {
  assert.deepEqual(replaceCells([["a.b axb"]], ".", "$&$&", { matchCase: false, wholeCell: false }), [[0, 0, "a$&$&b axb"]]);
});
