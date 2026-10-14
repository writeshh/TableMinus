// Run with `node --test src/` (Node strips the types itself). Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { BUNDLED, parseTheme, validValue } from "./theme-file.ts";

const theme = (t: unknown) => JSON.stringify(t);

test("parseTheme reads a valid theme", () => {
  const t = parseTheme(theme({ name: " Mine ", scheme: "dark", vars: { "--bg": "#101010", "--grid-size": "13px", "--editor-font": "\"JetBrains Mono\", monospace" } }));
  assert.deepEqual(t, { name: "Mine", scheme: "dark", vars: { "--bg": "#101010", "--grid-size": "13px", "--editor-font": "\"JetBrains Mono\", monospace" } });
});

test("parseTheme refuses unknown variables and fields", () => {
  assert.throws(() => parseTheme(theme({ name: "x", scheme: "light", vars: { "--evil": "#000" } })), /Unknown variable/);
  assert.throws(() => parseTheme(theme({ name: "x", scheme: "light", vars: {}, css: "body{}" })), /Unknown field/);
});

test("parseTheme refuses values that could smuggle CSS", () => {
  assert.throws(() => parseTheme(theme({ name: "x", scheme: "light", vars: { "--bg": "red; background: url(x)" } })), /Not a valid value/);
  assert.throws(() => parseTheme(theme({ name: "x", scheme: "light", vars: { "--grid-font": "a}body{color:red" } })), /Not a valid value/);
});

test("parseTheme refuses oversized and malformed files", () => {
  assert.throws(() => parseTheme(" ".repeat(64 * 1024 + 1)), /over 64 KB/);
  assert.throws(() => parseTheme("{"), /isn't JSON/);
  assert.throws(() => parseTheme(theme({ name: "x", scheme: "sepia", vars: {} })), /scheme/);
});

test("validValue caps sizes at 40 px", () => {
  assert.ok(validValue("px", "40px"));
  assert.ok(!validValue("px", "41px"));
  assert.ok(!validValue("px", "12"));
});

test("bundled themes pass their own validation", () => {
  for (const t of BUNDLED) assert.deepEqual(parseTheme(JSON.stringify(t)), t);
});
