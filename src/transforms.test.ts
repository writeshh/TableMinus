// Run with `node --test src/` (Node strips the types itself). Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { capitalize, convertIndent, transformText, trimTrailing } from "./transforms.ts";

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
