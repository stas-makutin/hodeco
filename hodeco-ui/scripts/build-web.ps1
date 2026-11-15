param(
    [switch]$NoOptimize,
    [switch]$NoCompress,
    [string]$WasmOpt = $env:WASM_OPT
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $root "Cargo.toml"
$dist = Join-Path $root "dist"
$wasm = Join-Path $root "target\wasm32-unknown-unknown\release\hodeco_ui.wasm"

Write-Host "Building hodeco-ui for wasm32..."
cargo build --manifest-path $manifest --lib --locked --target wasm32-unknown-unknown --release
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}
if (-not (Test-Path $wasm)) {
    throw "wasm artifact not found: $wasm"
}

if (Test-Path $dist) {
    Remove-Item -LiteralPath $dist -Recurse -Force
}
New-Item -ItemType Directory -Path $dist | Out-Null

Write-Host "Running wasm-bindgen..."
wasm-bindgen --target web --out-dir $dist --out-name hodeco-ui $wasm
if ($LASTEXITCODE -ne 0) {
    throw "wasm-bindgen failed with exit code $LASTEXITCODE"
}
Get-ChildItem -LiteralPath $dist -Filter "*.d.ts" | Remove-Item -Force
Copy-Item -LiteralPath (Join-Path $root "index.html") -Destination (Join-Path $dist "index.html")
$codecData = Join-Path $root "..\hodeco-codec\data"
Copy-Item -LiteralPath (Join-Path $codecData "zwave.json") -Destination (Join-Path $dist "zwave.json")
Copy-Item -LiteralPath (Join-Path $codecData "entities.json") -Destination (Join-Path $dist "entities.json")

if (-not $NoOptimize) {
    $wasmOptCommand = $null
    if ($WasmOpt) {
        if (Test-Path -LiteralPath $WasmOpt) {
            $wasmOptCommand = (Resolve-Path -LiteralPath $WasmOpt).Path
        } else {
            $command = Get-Command $WasmOpt -ErrorAction SilentlyContinue
            if ($null -ne $command) {
                $wasmOptCommand = $command.Source
            }
        }
        if ($null -eq $wasmOptCommand) {
            throw "wasm-opt was not found: $WasmOpt"
        }
    } else {
        $command = Get-Command wasm-opt -ErrorAction SilentlyContinue
        if ($null -ne $command) {
            $wasmOptCommand = $command.Source
        }
    }

    if ($null -eq $wasmOptCommand) {
        Write-Warning "wasm-opt not found; skipping size optimization."
        Write-Host "Install Binaryen or pass -WasmOpt <path> or set WASM_OPT."
    } else {
        Write-Host "Optimizing wasm with wasm-opt -Oz: $wasmOptCommand"
        $inputWasm = Join-Path $dist "hodeco-ui_bg.wasm"
        $outputWasm = Join-Path $dist "hodeco-ui_bg.wasm.opt"
        & $wasmOptCommand -Oz $inputWasm -o $outputWasm
        Move-Item -LiteralPath $outputWasm -Destination $inputWasm -Force
    }
}

if (-not $NoCompress) {
    function Write-Gzip([string]$Source, [string]$Target) {
        $input = [System.IO.File]::OpenRead($Source)
        try {
            $output = [System.IO.File]::Create($Target)
            try {
                $gzip = [System.IO.Compression.GZipStream]::new(
                    $output,
                    [System.IO.Compression.CompressionLevel]::Optimal
                )
                try {
                    $input.CopyTo($gzip)
                } finally {
                    $gzip.Dispose()
                }
            } finally {
                $output.Dispose()
            }
        } finally {
            $input.Dispose()
        }
    }

    Write-Host "Writing precompressed .gz and .br assets..."
    $brotli = Get-Command brotli -ErrorAction SilentlyContinue
    $python = $null
    $usePythonBrotli = $false
    if ($null -eq $brotli) {
        $python = Get-Command python -ErrorAction SilentlyContinue
        if ($null -ne $python) {
            & $python.Source -c "import brotli" 2>$null
            $usePythonBrotli = $LASTEXITCODE -eq 0
        }
    }
    $pythonBrotli = @'
import brotli
import sys

with open(sys.argv[1], 'rb') as source:
    data = source.read()
with open(sys.argv[2], 'wb') as target:
    target.write(brotli.compress(data, quality=11))
'@

    # JSON definitions are runtime-editable and must be served from the raw
    # file; precompressed sidecars would go stale after an edit.
    $compressible = @(".html", ".js", ".wasm", ".css", ".svg", ".txt")
    foreach ($file in Get-ChildItem -LiteralPath $dist -File) {
        if ($compressible -notcontains $file.Extension) {
            continue
        }
        Write-Gzip $file.FullName "$($file.FullName).gz"
        if ($null -ne $brotli) {
            & $brotli.Source -q 11 -f -o "$($file.FullName).br" $file.FullName
        } elseif ($usePythonBrotli) {
            & $python.Source -c $pythonBrotli $file.FullName "$($file.FullName).br"
            if ($LASTEXITCODE -ne 0) {
                $usePythonBrotli = $false
            }
        }
    }
    if ($null -eq $brotli -and -not $usePythonBrotli) {
        Write-Warning "brotli not found; only .gz sidecars were generated."
        Write-Host "Install the brotli CLI or Python's brotli module to generate .br files."
    }
} else {
    Write-Host "Skipping precompressed assets."
}

Get-ChildItem -LiteralPath $dist | Select-Object Name, Length
