import { test } from "node:test";
import assert from "node:assert/strict";
import { blocks, pendingCall, providerTurns, resultText, schemaText, splitArgs, type DiagramData } from "./ai-text.ts";

const data: DiagramData = {
  tables: [
    { name: "people", view: false, columns: [{ name: "id", data_type: "int", primary: true, nullable: false }, { name: "name", data_type: "text", primary: false, nullable: true }] },
    { name: "pets", view: false, columns: [{ name: "owner", data_type: "int", primary: false, nullable: true }] },
    { name: "names", view: true, columns: [{ name: "name", data_type: "text", primary: false, nullable: true }] },
  ],
  links: [{ table: "pets", columns: ["owner"], ref_table: "people", ref_columns: ["id"] }],
};

test("schemaText lists columns with types, keys and views", () => {
  assert.equal(
    schemaText(data),
    "people(id int primary key not null, name text)\npets(owner int); keys (owner) → people(id)\nview names(name text)",
  );
});

test("schemaText cuts a long schema and says how many tables were left out", () => {
  const text = schemaText(data, 60);
  assert.match(text, /^people\(/);
  assert.match(text, /…and 2 more tables, left out for length$/);
});

test("resultText gives tab-separated rows, NULLs and the row count", () => {
  const text = resultText({ columns: ["a", "b"], rows: [["1", null], ["x\ty", "2"]], affected: 0, truncated: false });
  assert.equal(text, "a\tb\n1\tNULL\nx y\t2\n(2 rows)");
});

test("resultText cuts at the limit and says more rows exist", () => {
  const rows = Array.from({ length: 100 }, (_, i) => [String(i)]);
  const text = resultText({ columns: ["n"], rows, affected: 0, truncated: false }, 40);
  assert.ok(text.length < 80);
  assert.match(text, /more not shown\)$/);
});

test("resultText reports affected rows for a statement without columns", () => {
  assert.equal(resultText({ columns: [], rows: [], affected: 3, truncated: false }), "3 rows affected");
});

test("blocks splits text and fenced code, keeping the language", () => {
  assert.deepEqual(blocks("Try this:\n```sql\nselect 1;\n```\nDone."), [
    { code: false, text: "Try this:", lang: "" },
    { code: true, text: "select 1;", lang: "sql" },
    { code: false, text: "Done.", lang: "" },
  ]);
});

test("blocks keeps an unclosed fence as code", () => {
  assert.deepEqual(blocks("```\nselect 2"), [{ code: true, text: "select 2", lang: "" }]);
});

test("pendingCall is the last turn's call, and none once a result follows", () => {
  const call = { id: "c", sql: "select 1" };
  assert.deepEqual(pendingCall([{ kind: "user", text: "q" }, { kind: "assistant", text: "", call }]), call);
  assert.equal(pendingCall([{ kind: "assistant", text: "", call }, { kind: "result", id: "c", text: "1" }]), null);
  assert.equal(pendingCall([{ kind: "assistant", text: "hi" }]), null);
});

test("providerTurns drops an agent's tool calls and empty answers", () => {
  const turns = [
    { kind: "user" as const, text: "q" },
    { kind: "assistant" as const, text: "" },
    { kind: "tool" as const, id: "t", title: "run_query", status: "completed" },
    { kind: "assistant" as const, text: "a" },
  ];
  assert.deepEqual(providerTurns(turns), [turns[0], turns[3]]);
});

test("splitArgs splits at spaces and keeps quoted spaces", () => {
  assert.deepEqual(splitArgs(' -y  @zed-industries/claude-code-acp "--name=a b" '), ["-y", "@zed-industries/claude-code-acp", "--name=a b"]);
  assert.deepEqual(splitArgs(""), []);
});
