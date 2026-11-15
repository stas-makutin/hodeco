#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

echo "Generating THIRD-PARTY-LICENSES.md (backend)..."
cargo about generate about.hbs -o THIRD-PARTY-LICENSES.md --workspace --locked --fail \
  --target x86_64-pc-windows-msvc --target x86_64-unknown-linux-gnu

echo "Generating hodeco-ui/THIRD-PARTY-LICENSES.md (frontend)..."
cargo about generate about.hbs -c about.toml -m hodeco-ui/Cargo.toml \
  -o hodeco-ui/THIRD-PARTY-LICENSES.md --workspace --locked --fail \
  --target x86_64-pc-windows-msvc --target x86_64-unknown-linux-gnu \
  --target aarch64-apple-darwin --target wasm32-unknown-unknown

# License files from crates may use CRLF; keep the checkout LF-only.
for file in THIRD-PARTY-LICENSES.md hodeco-ui/THIRD-PARTY-LICENSES.md; do
  if grep -q $'\r' "$file"; then
    tr -d '\r' < "$file" > "$file.tmp"
    mv "$file.tmp" "$file"
  fi
done
