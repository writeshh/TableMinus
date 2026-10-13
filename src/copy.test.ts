// Run with `node --test src/` (Node strips the types itself). Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { csv, inserts, markdown, plain } from "./copy.ts";

const cells = { columns: ["id", "note"], rows: [["1", "a,b"], ["2", null], ["3", 'say "hi"\nbye']] };

test("plain copies one cell exactly, even with a tab in it", () => {
  assert.equal(plain({ columns: ["v"], rows: [["a\tb"]] }), "a\tb");
});

test("plain quotes multi-cell values that would break the tab layout", () => {
  assert.equal(plain(cells), '1\ta,b\n2\tNULL\n3\t"say ""hi""\nbye"');
});

test("csv quotes commas, quotes and newlines and leaves NULL empty", () => {
  assert.equal(csv(cells), 'id,note\n1,"a,b"\n2,\n3,"say ""hi""\nbye"');
});

test("markdown escapes pipes and keeps each row on one line", () => {
  const md = markdown({ columns: ["a|b"], rows: [["x|y\nz"]] });
  assert.equal(md, "| a\\|b |\n| --- |\n| x\\|y z |");
});

test("inserts quote identifiers and literals for postgres", () => {
  const sql = inserts({ columns: ["id", "na\"me"], rows: [["1", "O'Hara"], ["2", null]] }, "public.people", "postgres");
  assert.equal(
    sql,
    `INSERT INTO "public"."people" ("id", "na""me") VALUES ('1', 'O''Hara');\nINSERT INTO "public"."people" ("id", "na""me") VALUES ('2', NULL);`,
  );
});

test("inserts double backslashes for mysql only", () => {
  const row = { columns: ["p"], rows: [["C:\\temp"]] };
  assert.equal(inserts(row, "t", "mysql"), "INSERT INTO `t` (`p`) VALUES ('C:\\\\temp');");
  assert.equal(inserts(row, "t", "sqlite"), `INSERT INTO "t" ("p") VALUES ('C:\\temp');`);
});
