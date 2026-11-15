$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

function Normalize-Eol([string]$Path) {
    $text = [IO.File]::ReadAllText($Path) -replace "`r`n", "`n"
    [IO.File]::WriteAllText($Path, $text, [Text.UTF8Encoding]::new($false))
}

Push-Location $root
try {
    Write-Host "Generating THIRD-PARTY-LICENSES.md (backend)..."
    cargo about generate about.hbs -o THIRD-PARTY-LICENSES.md --workspace --locked --fail `
        --target x86_64-pc-windows-msvc --target x86_64-unknown-linux-gnu
    if ($LASTEXITCODE -ne 0) {
        throw "cargo about failed with exit code $LASTEXITCODE"
    }

    Write-Host "Generating hodeco-ui/THIRD-PARTY-LICENSES.md (frontend)..."
    cargo about generate about.hbs -c about.toml -m hodeco-ui/Cargo.toml `
        -o hodeco-ui/THIRD-PARTY-LICENSES.md --workspace --locked --fail `
        --target x86_64-pc-windows-msvc --target x86_64-unknown-linux-gnu `
        --target aarch64-apple-darwin --target wasm32-unknown-unknown
    if ($LASTEXITCODE -ne 0) {
        throw "cargo about failed with exit code $LASTEXITCODE"
    }

    Normalize-Eol "THIRD-PARTY-LICENSES.md"
    Normalize-Eol "hodeco-ui/THIRD-PARTY-LICENSES.md"
} finally {
    Pop-Location
}
