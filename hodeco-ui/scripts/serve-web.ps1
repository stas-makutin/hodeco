param(
    [int]$Port = 8080
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
python -m http.server $Port --bind 127.0.0.1 --directory (Join-Path $root "dist")
