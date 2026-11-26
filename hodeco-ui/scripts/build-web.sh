#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dist="$root/dist"
wasm="$root/target/wasm32-unknown-unknown/release/hodeco_ui.wasm"
no_optimize=false
no_compress=false

for arg in "$@"; do
  case "$arg" in
    --no-optimize) no_optimize=true ;;
    --no-compress) no_compress=true ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

echo "Building hodeco-ui for wasm32..."
cargo build --manifest-path "$root/Cargo.toml" --lib --locked --target wasm32-unknown-unknown --release

if [[ ! -f "$wasm" ]]; then
  echo "wasm artifact not found: $wasm" >&2
  exit 1
fi

rm -rf "$dist"
mkdir -p "$dist"

echo "Running wasm-bindgen..."
wasm-bindgen --target web --out-dir "$dist" --out-name hodeco-ui "$wasm"
rm -f "$dist"/*.d.ts
cp "$root/index.html" "$dist/index.html"
cp "$root/../hodeco-codec/data/zwave.json" "$dist/zwave.json"
cp "$root/../hodeco-codec/data/entities.json" "$dist/entities.json"
cp "$root/../hodeco-codec/data/zigbee.json" "$dist/zigbee.json"

if [[ "$no_optimize" != true ]]; then
  wasm_opt_bin="${WASM_OPT:-wasm-opt}"
  if command -v "$wasm_opt_bin" >/dev/null 2>&1; then
    echo "Optimizing wasm with wasm-opt -Oz: $wasm_opt_bin"
    "$wasm_opt_bin" -Oz "$dist/hodeco-ui_bg.wasm" -o "$dist/hodeco-ui_bg.wasm.opt"
    mv "$dist/hodeco-ui_bg.wasm.opt" "$dist/hodeco-ui_bg.wasm"
  else
    echo "warning: wasm-opt not found; skipping size optimization" >&2
    echo "install Binaryen or set WASM_OPT=/path/to/wasm-opt" >&2
  fi
fi

if [[ "$no_compress" != true ]]; then
  echo "Writing precompressed .gz and .br assets..."
  have_brotli=false
  if command -v brotli >/dev/null 2>&1; then
    have_brotli=true
    brotli_command=(brotli -q 11 -f -o)
  else
    python_bin=""
    if command -v python3 >/dev/null 2>&1; then
      python_bin=python3
    elif command -v python >/dev/null 2>&1; then
      python_bin=python
    fi
    if [[ -n "$python_bin" ]] && "$python_bin" -c "import brotli" >/dev/null 2>&1; then
      have_brotli=true
    fi
  fi
  python_brotli='
import brotli
import sys

with open(sys.argv[1], "rb") as source:
    data = source.read()
with open(sys.argv[2], "wb") as target:
    target.write(brotli.compress(data, quality=11))
'
  # JSON definitions are runtime-editable and must be served from the raw
  # file; precompressed sidecars would go stale after an edit.
  for file in "$dist"/*; do
    [[ -f "$file" ]] || continue
    case "${file##*.}" in
      html|js|wasm|css|svg|txt) ;;
      *) continue ;;
    esac
    gzip -9 -n -c "$file" > "$file.gz"
    if [[ "$have_brotli" == true ]]; then
      if [[ -n "${brotli_command:-}" ]]; then
        "${brotli_command[@]}" "$file.br" "$file"
      else
        "$python_bin" -c "$python_brotli" "$file" "$file.br"
      fi
    fi
  done
  if [[ "$have_brotli" != true ]]; then
    echo "warning: brotli not found; only .gz sidecars were generated" >&2
    echo "install the brotli CLI or Python's brotli module for .br files" >&2
  fi
fi

ls -l "$dist"
