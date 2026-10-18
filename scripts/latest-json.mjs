#!/usr/bin/env node
// scripts/latest-json.mjs <bundle dir> <target triple>: writes <bundle dir>/latest.json, the file the app's updater
// reads (src-tauri/src/updater.rs), for the update artifact `tauri build` signed. The release's notes are the
// version's section of CHANGELOG.md. Upload it with the artifact to the release tagged v<version>.
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const [bundle, triple] = process.argv.slice(2);
if (!bundle || !triple) {
  console.error("usage: latest-json.mjs <bundle dir> <target triple>");
  process.exit(2);
}
const root = fileURLToPath(new URL("..", import.meta.url)); // decoded: the folder may have spaces
const version = JSON.parse(readFileSync(join(root, "src-tauri/tauri.conf.json"), "utf8")).version;
const repo = "https://github.com/writeshh/TableMinus";

// Tauri's platform names: darwin-aarch64, linux-x86_64, windows-x86_64…
const arch = triple.split("-")[0];
const os = triple.includes("apple-darwin") ? "darwin" : triple.includes("windows") ? "windows" : "linux";

// The signed update artifact: the .app as .tar.gz on macOS, the AppImage on Linux, the NSIS installer on Windows.
const [dir, match] = {
  darwin: ["macos", (f) => f.endsWith(".app.tar.gz")],
  linux: ["appimage", (f) => f.endsWith(".AppImage")],
  windows: ["nsis", (f) => f.endsWith("-setup.exe")],
}[os];
const folder = join(bundle, dir);
const artifact = existsSync(folder) && readdirSync(folder).find((f) => match(f) && existsSync(join(folder, `${f}.sig`)));
if (!artifact) {
  console.error(`no signed update artifact in ${folder}; was the signing key set?`);
  process.exit(1);
}

/** The version's section of CHANGELOG.md, without its heading. */
function notes() {
  const text = readFileSync(join(root, "CHANGELOG.md"), "utf8");
  const start = text.search(new RegExp(`^## \\[?${version.replaceAll(".", "\\.")}\\]?(\\s|$)`, "m"));
  if (start < 0) return `TableMinus ${version}`;
  const body = text.slice(start).split("\n").slice(1).join("\n");
  const end = body.search(/^## /m);
  return (end < 0 ? body : body.slice(0, end)).trim();
}

const latest = {
  version,
  notes: notes(),
  pub_date: new Date().toISOString(),
  platforms: {
    [`${os}-${arch}`]: {
      signature: readFileSync(join(folder, `${artifact}.sig`), "utf8").trim(),
      url: `${repo}/releases/download/v${version}/${artifact.replaceAll(" ", ".")}`,
    },
  },
};
writeFileSync(join(bundle, "latest.json"), JSON.stringify(latest, null, 2) + "\n");
console.log(`${join(bundle, "latest.json")}: ${os}-${arch} → ${artifact}`);
