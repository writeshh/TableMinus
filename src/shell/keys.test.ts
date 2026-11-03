// Run with `node --test src/` (Node strips the types itself). Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { conflicts, fromEvent, keys, label, resolve, spoken } from "./keys.ts";

const press = (code: string, mods: Partial<Record<"ctrlKey" | "altKey" | "shiftKey" | "metaKey", boolean>> = {}) =>
  ({ ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, code, ...mods });

test("resolve turns Mod into Cmd on macOS and Ctrl elsewhere, with modifiers in order", () => {
  assert.equal(resolve("Shift+Mod+KeyT", true), "Shift+Meta+KeyT");
  assert.equal(resolve("Mod+Shift+KeyT", false), "Ctrl+Shift+KeyT");
});

test("resolve refuses a binding without a key", () => {
  assert.equal(resolve("Mod+Shift", true), "");
  assert.equal(resolve("", true), "");
});

test("fromEvent matches what resolve makes of the same keys", () => {
  assert.equal(fromEvent(press("BracketRight", { metaKey: true, shiftKey: true })), resolve("Mod+Shift+BracketRight", true));
  assert.equal(fromEvent(press("Tab", { ctrlKey: true })), "Ctrl+Tab");
});

test("fromEvent ignores a modifier pressed on its own", () => {
  assert.equal(fromEvent(press("MetaLeft", { metaKey: true })), "");
  assert.equal(fromEvent(press("ShiftRight", { shiftKey: true })), "");
});

test("label shows symbols on macOS and words elsewhere", () => {
  assert.equal(label("Shift+Meta+KeyT", true), "⇧⌘T");
  assert.equal(label("Ctrl+Shift+BracketLeft", false), "Ctrl+Shift+[");
  assert.equal(label("Meta+Digit1", true), "⌘1");
});

test("keys splits a binding into one entry per keycap", () => {
  assert.deepEqual(keys("Shift+Meta+KeyT", true), ["⇧", "⌘", "T"]);
  assert.deepEqual(keys("Ctrl+Shift+Digit1", false), ["Ctrl", "Shift", "1"]);
  assert.deepEqual(keys("", true), []);
});

test("spoken names the modifiers in words", () => {
  assert.equal(spoken("Shift+Meta+KeyC", true), "Shift + Command + C");
  assert.equal(spoken("Alt+Ctrl+Enter", true), "Option + Control + ↩");
  assert.equal(spoken("Ctrl+KeyW", false), "Ctrl + W");
});

test("conflicts lists bindings shared by several commands, not one command's own repeats", () => {
  const found = conflicts({ a: ["Meta+KeyR", "Meta+KeyR"], b: ["Meta+KeyR"], c: ["Meta+KeyT"] });
  assert.deepEqual([...found], [["Meta+KeyR", ["a", "b"]]]);
});
