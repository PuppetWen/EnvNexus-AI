param(
    [switch]$Release
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $ProjectRoot
try {
    & ".\node_modules\.bin\tsc.cmd" --noEmit
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\node_modules\.bin\vitest.cmd" run
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\node_modules\.bin\vite.cmd" build
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\scripts\Invoke-Rust.ps1" -Task fmt
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\scripts\Invoke-Rust.ps1" -Task test -Release:$Release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\scripts\Invoke-Rust.ps1" -Task clippy -Release:$Release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    Pop-Location
}
