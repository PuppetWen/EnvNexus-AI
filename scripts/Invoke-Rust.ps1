param(
    [ValidateSet("check", "test", "clippy", "build", "fmt", "tauri", "environment", "live-sources", "live-added-sources", "live-install", "release-assets")]
    [string]$Task = "check",
    [switch]$Release,
    [string]$LlvmBin,
    [string]$ResourceCompiler,
    [string]$TauriExecutable
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
& (Join-Path $PSScriptRoot 'Use-ProjectCache.ps1')
$ManifestPath = Join-Path $ProjectRoot "src-tauri\Cargo.toml"
$ProjectRustup = Join-Path $ProjectRoot ".devtools\rustup"
$ProjectCargo = Join-Path $ProjectRoot ".devtools\cargo"
$ProjectToolchain = Join-Path $ProjectRustup "toolchains\stable-x86_64-pc-windows-msvc"
$ProjectToolchainBin = Join-Path $ProjectToolchain "bin"
$CargoExecutable = Join-Path $ProjectToolchainBin "cargo.exe"

function Resolve-ExistingFile {
    param([string[]]$Candidates)
    foreach ($Candidate in $Candidates) {
        if ($Candidate -and (Test-Path -LiteralPath $Candidate -PathType Leaf)) {
            return [System.IO.Path]::GetFullPath($Candidate)
        }
    }
}

if (Test-Path -LiteralPath $CargoExecutable -PathType Leaf) {
    $env:RUSTUP_HOME = $ProjectRustup
    $env:CARGO_HOME = $ProjectCargo
    $env:RUSTC = Join-Path $ProjectToolchainBin "rustc.exe"
    $env:RUSTDOC = Join-Path $ProjectToolchainBin "rustdoc.exe"
    $env:PATH = "$ProjectToolchainBin;$env:PATH"
} else {
    $CargoExecutable = (Get-Command cargo.exe -ErrorAction Stop).Source
}

# Formatting does not need a Windows linker or SDK. Check rather than rewrite sources.
if ($Task -eq "fmt") {
    & $CargoExecutable fmt --manifest-path $ManifestPath -- --check
    exit $LASTEXITCODE
}

$ProjectSdk = Join-Path $ProjectRoot ".devtools\xwin-sdk"
$HasProjectSdk = Test-Path -LiteralPath (Join-Path $ProjectSdk "sdk\lib\um\x86_64\kernel32.lib") -PathType Leaf
if ($HasProjectSdk) {
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
}

# Respect an explicitly configured linker. Otherwise prefer the installed MSVC
# toolset, with Rust's bundled linker available when using the project xwin SDK.
$MsvcBin = $null
if (-not $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER) {
    $VsWhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    $VsRoots = @()
    if (Test-Path -LiteralPath $VsWhere -PathType Leaf) {
        $VsRoots = @(& $VsWhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
    }
    $VsRoots += @(
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio\2022\Community"),
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio\2022\BuildTools")
    )
    $MsvcBin = $VsRoots | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | ForEach-Object {
        $Toolsets = Join-Path $_ "VC\Tools\MSVC"
        if (Test-Path -LiteralPath $Toolsets) {
            Get-ChildItem -LiteralPath $Toolsets -Directory | Sort-Object Name -Descending | ForEach-Object {
                Join-Path $_.FullName "bin\Hostx64\x64"
            }
        }
    } | Where-Object { Test-Path -LiteralPath (Join-Path $_ "link.exe") -PathType Leaf } | Select-Object -First 1
    if ($MsvcBin) {
        $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = Join-Path $MsvcBin "link.exe"
        $env:PATH = "$MsvcBin;$env:PATH"
    } elseif ($HasProjectSdk) {
        $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = Resolve-ExistingFile @(
            (Join-Path $ProjectToolchain "lib\rustlib\x86_64-pc-windows-msvc\bin\rust-lld.exe")
        )
    }
}
if (-not $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER -and -not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
    throw "MSVC linker not found. Use a Visual Studio developer shell, or provide the project xwin SDK and Rust's bundled rust-lld.exe."
}

$LlvmCandidates = @($LlvmBin, (Join-Path $ProjectRoot ".devtools\llvm\bin"), (Join-Path $env:ProgramFiles "LLVM\bin"))
if ($LlvmBin -and -not (Test-Path -LiteralPath (Join-Path $LlvmBin "clang-cl.exe") -PathType Leaf)) {
    throw "clang-cl.exe was not found in -LlvmBin: $LlvmBin"
}
$Clang = Resolve-ExistingFile @($LlvmCandidates | Where-Object { $_ } | ForEach-Object { Join-Path $_ "clang-cl.exe" })
if (-not $Clang) {
    $ClangCommand = Get-Command clang-cl.exe -ErrorAction SilentlyContinue
    if ($ClangCommand) { $Clang = $ClangCommand.Source }
}
if ($Clang) {
    if ($LlvmBin -or -not $env:CC) { $env:CC = $Clang }
    if ($LlvmBin -or -not $env:CXX) { $env:CXX = $Clang }
    if ($LlvmBin -or -not $env:AR) {
        $env:AR = Resolve-ExistingFile @((Join-Path (Split-Path -Parent $Clang) "llvm-lib.exe"))
    }
}
if (-not $MsvcBin -and -not $env:CC -and -not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    throw "C/C++ compiler not found. Use a Visual Studio developer shell, set CC/CXX/AR, or pass -LlvmBin with clang-cl.exe and llvm-lib.exe."
}

if ($ResourceCompiler) {
    if (-not (Test-Path -LiteralPath $ResourceCompiler -PathType Leaf)) { throw "Resource compiler not found: $ResourceCompiler" }
    $env:RC = [System.IO.Path]::GetFullPath($ResourceCompiler)
} elseif (-not $env:RC) {
    $RcCommand = Get-Command rc.exe -ErrorAction SilentlyContinue
    if ($RcCommand) { $env:RC = $RcCommand.Source }
    if (-not $env:RC) {
        $SdkBins = @(
            (Join-Path $ProjectRoot ".devtools\WindowsKits\bin"),
            (Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin")
        )
        $env:RC = $SdkBins | Where-Object { Test-Path -LiteralPath $_ } | ForEach-Object {
            Get-ChildItem -LiteralPath $_ -Directory | Sort-Object Name -Descending | ForEach-Object {
                Join-Path $_.FullName "x64\rc.exe"
            }
        } | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    }
}
if (-not $env:RC -or -not (Test-Path -LiteralPath $env:RC -PathType Leaf)) {
    throw "Windows resource compiler not found. Install the Windows SDK, set RC, or pass -ResourceCompiler."
}
$env:PATH = "$(Split-Path -Parent $env:RC);$env:PATH"

if ($Task -eq "environment") {
    [ordered]@{
        cargo = $CargoExecutable
        rustc = $env:RUSTC
        linker = $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER
        cc = $env:CC
        cxx = $env:CXX
        ar = $env:AR
        rc = $env:RC
        projectSdk = $HasProjectSdk
    } | ConvertTo-Json
    exit 0
}

if ($Task -eq "tauri") {
    if (-not $TauriExecutable) {
        $TauriExecutable = Resolve-ExistingFile @(
            (Join-Path $ProjectRoot ".devtools\tauri-cli\bin\cargo-tauri.exe"),
            (Join-Path $ProjectCargo "bin\cargo-tauri.exe"),
            (Join-Path $ProjectRoot "node_modules\.bin\tauri.cmd")
        )
    }
    if (-not $TauriExecutable -or -not (Test-Path -LiteralPath $TauriExecutable -PathType Leaf)) {
        throw "Tauri CLI not found. Install the project frontend dependencies or provide -TauriExecutable."
    }
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
        $LocalSigningKey = Join-Path $ProjectRoot ".devtools\updater\envnexus-ai.key"
        if (-not (Test-Path -LiteralPath $LocalSigningKey -PathType Leaf)) {
            throw "Updater signing key not found. Set TAURI_SIGNING_PRIVATE_KEY or restore .devtools\updater\envnexus-ai.key."
        }
        $env:TAURI_SIGNING_PRIVATE_KEY = [System.IO.File]::ReadAllText($LocalSigningKey)
    }
    Push-Location $ProjectRoot
    try {
        & $TauriExecutable build --ci
        exit $LASTEXITCODE
    } finally {
        Pop-Location
    }
}

$CargoTask = if ($Task -in @("live-sources", "live-added-sources", "live-install", "release-assets")) { "test" } else { $Task }
$CargoArguments = @($CargoTask, "--manifest-path", $ManifestPath)
if ($Release -and $CargoTask -in @("build", "test", "clippy")) { $CargoArguments += "--release" }
if ($Task -eq "build") { $CargoArguments += @("--features", "tauri/custom-protocol") }
if ($Task -eq "clippy") { $CargoArguments += @("--all-targets", "--", "-D", "warnings") }
if ($Task -eq "test") { $CargoArguments += @("--", "--nocapture") }
$LiveTests = @{
    "live-sources" = "live_official_catalogs_return_windows_downloads"
    "live-added-sources" = "live_added_catalogs_return_windows_downloads"
    "live-install" = "live_python_install_transaction_commits_verified_version"
    "release-assets" = "built_release_assets_match_signed_metadata"
}
if ($LiveTests.ContainsKey($Task)) {
    $CargoArguments += @($LiveTests[$Task], "--", "--ignored", "--nocapture")
}

& $CargoExecutable @CargoArguments
exit $LASTEXITCODE
