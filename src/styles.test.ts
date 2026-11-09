import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

// Text sizes, corner radii, shadows and colours come from the tokens on :root in styles.css, so every screen uses
// the same few. diagram.ts is left out: its SVG style goes into exported files, where only its colours are resolved.
// connections.ts is left out: its colours are the choices a connection's tag can take, which are data.
const dir = import.meta.dirname;
const files = (readdirSync(dir, { recursive: true }) as string[])
  .filter((f) => /\.(css|ts)$/.test(f) && !f.endsWith(".test.ts") && !f.endsWith(".d.ts") && f !== join("database", "diagram.ts") && f !== join("connections", "connections.ts"));

const RAW = [
  /font-size:\s*\d[\d.]*px/,
  /fontSize:\s*"\d[\d.]*px/,
  /font:\s*(?:(?:italic|bold|\d00)\s+)*\d[\d.]*px/,
  /border-radius:\s*(?!999px)\d[\d.]*px/,
  /[:(,]\s*#[0-9a-fA-F]{3,8}\b/, // a colour value, not an id selector
  /\brgba?\(/,
];
/** A custom property's definition (`--series-1: #…;`) names a colour on purpose; the rest of the line is checked. */
const definitions = /--[\w-]+:[^;]*;?/g;

test("text sizes, corner radii, shadows and colours use the tokens", () => {
  const found: string[] = [];
  for (const f of files) {
    let text = readFileSync(join(dir, f), "utf8");
    if (f === "styles.css") {
      const end = text.indexOf("\n}\n"); // the :root block defines the tokens; blank it, keeping line numbers
      text = text.slice(0, end).replace(/[^\n]/g, "") + text.slice(end);
    }
    text.split("\n").forEach((line, i) => {
      if (RAW.some((r) => r.test(line.replace(definitions, "")))) found.push(`${f}:${i + 1}: ${line.trim().slice(0, 100)}`);
    });
  }
  assert.deepEqual(found, [], "use the var(--text-*), var(--radius-*) and var(--shadow-*) tokens, or a colour variable, from styles.css :root");
});
