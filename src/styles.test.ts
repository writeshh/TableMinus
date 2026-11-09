import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

// Text sizes and corner radii come from the tokens on :root in styles.css, so every screen uses the same few.
// diagram.ts is left out: its SVG style goes into exported files, where only its colours are resolved.
const dir = import.meta.dirname;
const files = (readdirSync(dir, { recursive: true }) as string[])
  .filter((f) => /\.(css|ts)$/.test(f) && !f.endsWith(".test.ts") && !f.endsWith(".d.ts") && f !== join("database", "diagram.ts"));

const RAW = [
  /font-size:\s*\d[\d.]*px/,
  /fontSize:\s*"\d[\d.]*px/,
  /font:\s*(?:(?:italic|bold|\d00)\s+)*\d[\d.]*px/,
  /border-radius:\s*(?!999px)\d[\d.]*px/,
];

test("text sizes and corner radii use the tokens", () => {
  const found: string[] = [];
  for (const f of files) {
    let text = readFileSync(join(dir, f), "utf8");
    if (f === "styles.css") {
      const end = text.indexOf("\n}\n"); // the :root block defines the tokens; blank it, keeping line numbers
      text = text.slice(0, end).replace(/[^\n]/g, "") + text.slice(end);
    }
    text.split("\n").forEach((line, i) => {
      if (RAW.some((r) => r.test(line))) found.push(`${f}:${i + 1}: ${line.trim().slice(0, 100)}`);
    });
  }
  assert.deepEqual(found, [], "use var(--text-*) or var(--radius-*) from styles.css :root");
});
