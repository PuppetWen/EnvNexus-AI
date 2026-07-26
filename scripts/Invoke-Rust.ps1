param(
    [ValidateSet("check", "test", "clippy", "build", "fmt", "tauri", "live-sources", "live-added-sources", "live-install")]
    [string]$Task = "check",
    [switch]$Release
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$ManifestPath = Join-Path $ProjectRoot "src-tauri\Cargo.toml"
$ProjectRustup = Join-Path $ProjectRoot ".devtools\rustup"
$ProjectCargo = Join-Path $ProjectRoot ".devtools\cargo"
$ProjectToolchain = Join-Path $ProjectRustup "toolchains\stable-x86_64-pc-windows-msvc\bin"
$CargoExecutable = Join-Path $ProjectToolchain "cargo.exe"

if (Test-Path -LiteralPath $CargoExecutable) {
    $env:RUSTUP_HOME = $ProjectRustup
    $env:CARGO_HOME = $ProjectCargo
    $env:RUSTC = Join-Path $ProjectToolchain "rustc.exe"
    $env:RUSTDOC = Join-Path $ProjectToolchain "rustdoc.exe"
    $env:PATH = "$ProjectToolchain;$env:PATH"
} else {
    $CargoCommand = Get-Command cargo.exe -ErrorAction Stop
    $CargoExecutable = $CargoCommand.Source
}

$MsvcRoots = @(
    "D:\Environment\BuildTools\VC\Tools\MSVC",
    "E:\DevelopmentTools\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC",
    "E:\DevelopmentTools\Microsoft Visual Studio\2022\Enterprise\VC\Tools\MSVC",
    "${env:ProgramFiles}\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC",
    "${env:ProgramFiles}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC"
)
$MsvcBin = $MsvcRoots |
    Where-Object { Test-Path -LiteralPath $_ } |
    ForEach-Object { Get-ChildItem -LiteralPath $_ -Directory | Sort-Object Name -Descending | Select-Object -First 1 } |
    ForEach-Object { Join-Path $_.FullName "bin\Hostx64\x64" } |
    Where-Object { Test-Path -LiteralPath (Join-Path $_ "link.exe") } |
    Select-Object -First 1
if (-not $MsvcBin) {
    throw "MSVC x64 linker not found. Install the Visual Studio 2022 Desktop development with C++ workload."
}
$env:PATH = "$MsvcBin;$env:PATH"

$RcCandidates = @(
    "E:\DevelopmentTools\Microsoft Visual Studio\Shared\NuGetPackages",
    "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
) | Where-Object { Test-Path -LiteralPath $_ }
$RcExecutable = $RcCandidates |
    ForEach-Object { Get-ChildItem -LiteralPath $_ -Filter rc.exe -File -Recurse -ErrorAction SilentlyContinue } |
    Where-Object { $_.DirectoryName -match "\\x64$" } |
    Sort-Object FullName -Descending |
    Select-Object -First 1
if (-not $RcExecutable) {
    throw "Windows SDK resource compiler RC.EXE not found. Install a Windows 10 or 11 SDK."
}
$env:RC = $RcExecutable.FullName
$env:PATH = "$($RcExecutable.DirectoryName);$env:PATH"

$ProjectSdk = Join-Path $ProjectRoot ".devtools\xwin-sdk"
if (Test-Path -LiteralPath (Join-Path $ProjectSdk "sdk\lib\um\x86_64\kernel32.lib")) {
    $env:LIB = @(
        (Join-Path $ProjectSdk "crt\lib\x86_64"),
        (Join-Path $ProjectSdk "sdk\lib\um\x86_64"),
        (Join-Path $ProjectSdk "sdk\lib\ucrt\x86_64")
    ) -join ";"
    $env:INCLUDE = @(
        (Join-Path $ProjectSdk "crt\include"),
        (Join-Path $ProjectSdk "sdk\include\ucrt"),
        (Join-Path $ProjectSdk "sdk\include\um"),
        (Join-Path $ProjectSdk "sdk\include\shared"),
        (Join-Path $ProjectSdk "sdk\include\winrt")
    ) -join ";"
    $env:WindowsSdkDir = Join-Path $ProjectSdk "sdk"
    $env:WindowsSDKVersion = "10.0.26100.0"
}

if ($Task -eq "tauri") {
    $TauriExecutable = Join-Path $ProjectRoot "node_modules\.bin\tauri.cmd"
    if (-not (Test-Path -LiteralPath $TauriExecutable)) {
        throw "Tauri CLI not found. Run pnpm install first."
    }
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
        $LocalSigningKey = Join-Path $ProjectRoot ".devtools\updater\envnexus-ai.key"
        if (-not (Test-Path -LiteralPath $LocalSigningKey)) {
            throw "Updater signing key not found. Set TAURI_SIGNING_PRIVATE_KEY or restore .devtools\updater\envnexus-ai.key."
        }
        $env:TAURI_SIGNING_PRIVATE_KEY = [System.IO.File]::ReadAllText($LocalSigningKey)
    }
    Push-Location $ProjectRoot
    try {
        & $TauriExecutable build --ci
        exit $LASTEXITCODE
    }
    finally {
        Pop-Location
    }
}

$CargoTask = if ($Task -in @("live-sources", "live-added-sources", "live-install")) { "test" } else { $Task }
$CargoArguments = @($CargoTask, "--manifest-path", $ManifestPath)
if ($Release -and $Task -in @("build", "test", "clippy")) {
    $CargoArguments += "--release"
}
if ($Task -eq "clippy") {
    $CargoArguments += @("--all-targets", "--", "-D", "warnings")
}
if ($Task -eq "live-sources") {
    $CargoArguments += @(
        "live_official_catalogs_return_windows_downloads",
        "--",
        "--ignored",
        "--nocapture"
    )
}
if ($Task -eq "live-added-sources") {
    $CargoArguments += @(
        "live_added_catalogs_return_windows_downloads",
        "--",
        "--ignored",
        "--nocapture"
    )
}
if ($Task -eq "live-install") {
    $CargoArguments += @(
        "live_python_install_transaction_commits_verified_version",
        "--",
        "--ignored",
        "--nocapture"
    )
}

& $CargoExecutable @CargoArguments
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
