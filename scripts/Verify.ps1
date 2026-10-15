param(
    [switch]$Release,
    [string]$LlvmBin,
    [string]$ResourceCompiler
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
& (Join-Path $PSScriptRoot 'Use-ProjectCache.ps1')
Push-Location $ProjectRoot
try {
    & node --test (Join-Path $PSScriptRoot 'Prepare-Portable.nodecheck.mjs')
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\node_modules\.bin\tsc.cmd" --noEmit
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\node_modules\.bin\vitest.cmd" run
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\node_modules\.bin\vite.cmd" build
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    $RustOptions = @{}
    if ($LlvmBin) { $RustOptions.LlvmBin = $LlvmBin }
    if ($ResourceCompiler) { $RustOptions.ResourceCompiler = $ResourceCompiler }

    & ".\scripts\Invoke-Rust.ps1" -Task fmt @RustOptions
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\scripts\Invoke-Rust.ps1" -Task test -Release:$Release @RustOptions
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    & ".\scripts\Invoke-Rust.ps1" -Task clippy -Release:$Release @RustOptions
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    Pop-Location
}
