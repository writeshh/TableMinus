#!/usr/bin/env bash
# Builds and bundles TableMinus: checks, the MCP bridge, then the app (.app and .dmg on macOS).
#
#   scripts/build.sh                 checks, audit, then bundle for this machine
#   scripts/build.sh --skip-checks   bundle only
#   scripts/build.sh --target x86_64-apple-darwin   cross-build (install it with rustup target add first)
#
# The bridge goes in through src-tauri/tauri.bundle.json rather than tauri.conf.json: Tauri refuses to build when an
# external binary is missing, and plain `cargo test` / `tauri dev` shouldn't need the bridge built first.
set -euo pipefail

cd "$(dirname "$0")/.."

checks=1
target=""
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-checks) checks=0 ;;
    --target) target="${2:?--target needs a target triple}"; shift ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done

# rustup's toolchain isn't always on PATH in non-login shells.
if ! command -v cargo >/dev/null; then
  for d in "$HOME/.cargo/bin" "$HOME"/.rustup/toolchains/stable-*/bin; do
    [ -x "$d/cargo" ] && PATH="$d:$PATH"
  done
fi
command -v cargo >/dev/null || { echo "cargo not found; install Rust with rustup" >&2; exit 1; }
[ -d "$HOME/.cargo/bin" ] && PATH="$PATH:$HOME/.cargo/bin" # cargo-deny, cargo-audit

[ -d node_modules ] || npm ci

if [ "$checks" = 1 ]; then
  echo "==> npm run check"
  npm run check
  if command -v cargo-deny >/dev/null && command -v cargo-audit >/dev/null; then
    echo "==> npm run audit"
    npm run audit
  else
    echo "==> skipping the audit: cargo install --locked cargo-deny cargo-audit to enable it" >&2
  fi
fi

triple="${target:-$(rustc -vV | sed -n 's/^host: //p')}"
echo "==> MCP bridge for $triple"
(cd src-tauri && cargo build --release --locked --bin tableminus-mcp ${target:+--target "$target"})
out="src-tauri/target/${target:+$target/}release/tableminus-mcp"
[ -f "$out.exe" ] && { out="$out.exe"; ext=".exe"; } || ext=""
mkdir -p src-tauri/binaries
# Tauri looks for <name>-<target triple>; the bundle drops the suffix, leaving it next to the app's executable.
cp "$out" "src-tauri/binaries/tableminus-mcp-$triple$ext"

echo "==> bundling"
npm run tauri build -- --config src-tauri/tauri.bundle.json ${target:+--target "$target"}

bundle="src-tauri/target/${target:+$target/}release/bundle"
echo
echo "Done. Bundles:"
find "$bundle" -maxdepth 2 \( -name '*.app' -o -name '*.dmg' -o -name '*.AppImage' -o -name '*.deb' -o -name '*.msi' -o -name '*.exe' \) \
  -newer "src-tauri/binaries/tableminus-mcp-$triple$ext" -print
