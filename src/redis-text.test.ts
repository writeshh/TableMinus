import { test } from "node:test";
import assert from "node:assert/strict";
import { formatReply, formatTtl } from "./redis-text.ts";

test("formatReply prints scalars as redis-cli does", () => {
  assert.equal(formatReply({ kind: "status", text: "OK" }), "OK");
  assert.equal(formatReply({ kind: "error", text: "WRONGTYPE no" }), "(error) WRONGTYPE no");
  assert.equal(formatReply({ kind: "int", value: 3 }), "(integer) 3");
  assert.equal(formatReply({ kind: "bulk", text: null }), "(nil)");
  assert.equal(formatReply({ kind: "bulk", text: 'say "hi"\n' }), '"say \\"hi\\"\\n"');
});

test("formatReply numbers array items and indents nested ones", () => {
  const r = { kind: "array" as const, items: [
    { kind: "bulk" as const, text: "a" },
    { kind: "array" as const, items: [{ kind: "int" as const, value: 1 }, { kind: "bulk" as const, text: "b" }] },
  ] };
  assert.equal(formatReply(r), '1) "a"\n2) 1) (integer) 1\n   2) "b"');
  assert.equal(formatReply({ kind: "array", items: [] }), "(empty array)");
});

test("formatTtl shows the two largest units", () => {
  assert.equal(formatTtl(-1), "No expiry");
  assert.equal(formatTtl(-2), "Expired");
  assert.equal(formatTtl(1500), "2s");
  assert.equal(formatTtl(3_723_000), "1h 2m");
  assert.equal(formatTtl(90_061_000), "1d 1h");
});
