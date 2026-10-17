// A TableMinus plugin. Each action's function gets `input` and returns what to do with the result:
// { clipboard: "text" }, { insert: "SQL for the editor" }, { message: "text" }, or a string (a message).
//
// input for "rows" actions: { table, columns: ["id", ...], rows: [{ id: "1", ... }] } (values are strings or null)
// input for "table" actions: { table, engine }
// query(sql) runs read-only SQL on the open connection and returns rows as objects; only when the plugin was
// granted the "query" permission at install.

function copyJson(input) {
  return { clipboard: JSON.stringify(input.rows, null, 2) };
}

function countRows(input) {
  if (!canQuery) return "Grant this plugin the query permission to count rows.";
  const quote = input.engine === "mysql" || input.engine === "mariadb" ? "`" : '"';
  const name = input.table.split(".").map((part) => quote + part.split(quote).join(quote + quote) + quote).join(".");
  const [row] = query(`select count(*) as n from ${name}`);
  return `${input.table} has ${row.n} rows`;
}
