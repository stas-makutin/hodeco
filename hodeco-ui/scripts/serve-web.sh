#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 -m http.server "${1:-8080}" --bind 127.0.0.1 --directory "$root/dist"
