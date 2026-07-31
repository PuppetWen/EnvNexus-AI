param(
    [string]$ReleaseNotes
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$Configuration = Get-Content -LiteralPath (Join-Path $ProjectRoot "src-tauri\tauri.conf.json") -Raw -Encoding UTF8 | ConvertFrom-Json
$Version = [string]$Configuration.version
if (-not $ReleaseNotes) {
    $ReleaseNotes = "EnvNexus AI $Version release."
}
$Tag = "v$Version"
$ReleaseDirectory = Join-Path $ProjectRoot "release"
$ExecutableSource = Join-Path $ProjectRoot "src-tauri\target\release\envnexus-ai.exe"
$InstallerSource = Join-Path $ProjectRoot "src-tauri\target\release\bundle\nsis\EnvNexus AI_${Version}_x64-setup.exe"
$SignatureSource = "$InstallerSource.sig"

foreach ($RequiredFile in @($ExecutableSource, $InstallerSource, $SignatureSource)) {
    if (-not (Test-Path -LiteralPath $RequiredFile -PathType Leaf)) {
        throw "Release input not found: $RequiredFile"
    }
}

New-Item -ItemType Directory -Path $ReleaseDirectory -Force | Out-Null
$PortableName = "EnvNexus-AI_${Version}_x64-portable.exe"
$InstallerName = "EnvNexus-AI_${Version}_x64-setup.exe"
$SignatureName = "$InstallerName.sig"
$PortablePath = Join-Path $ReleaseDirectory $PortableName
$InstallerPath = Join-Path $ReleaseDirectory $InstallerName
$SignaturePath = Join-Path $ReleaseDirectory $SignatureName
$PortableSignaturePath = "$PortablePath.sig"

Copy-Item -LiteralPath $ExecutableSource -Destination $PortablePath -Force
Copy-Item -LiteralPath $InstallerSource -Destination $InstallerPath -Force
Copy-Item -LiteralPath $SignatureSource -Destination $SignaturePath -Force

if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
    $LocalSigningKey = Join-Path $ProjectRoot ".devtools\updater\envnexus-ai.key"
    if (-not (Test-Path -LiteralPath $LocalSigningKey -PathType Leaf)) {
        throw "Updater signing key not found. Set TAURI_SIGNING_PRIVATE_KEY or restore .devtools\updater\envnexus-ai.key."
    }
    $env:TAURI_SIGNING_PRIVATE_KEY = [System.IO.File]::ReadAllText($LocalSigningKey)
}
$TauriExecutable = Join-Path $ProjectRoot "node_modules\.bin\tauri.cmd"
if (-not (Test-Path -LiteralPath $TauriExecutable -PathType Leaf)) {
    throw "Tauri CLI not found. Run pnpm install first."
}
Remove-Item -LiteralPath $PortableSignaturePath -Force -ErrorAction SilentlyContinue
if ([string]::IsNullOrEmpty($env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD)) {
    # The signer prompts interactively when the password option is omitted.
    # Pass an explicit empty password for an unencrypted local signing key.
    & $TauriExecutable signer sign --password= $PortablePath
} else {
    & $TauriExecutable signer sign $PortablePath
}
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $PortableSignaturePath -PathType Leaf)) {
    throw "Portable updater signature generation failed."
}

$PortableHash = (Get-FileHash -LiteralPath $PortablePath -Algorithm SHA256).Hash.ToLowerInvariant()
$InstallerHash = (Get-FileHash -LiteralPath $InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()

$Latest = [ordered]@{
    version = $Version
    notes = $ReleaseNotes
    pub_date = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = [ordered]@{
        "windows-x86_64" = [ordered]@{
            signature = [System.IO.File]::ReadAllText($SignaturePath).Trim()
            url = "https://github.com/PuppetWen/EnvNexus-AI/releases/download/$Tag/$InstallerName"
            sha256 = $InstallerHash
        }
    }
    portable = [ordered]@{
        "windows-x86_64" = [ordered]@{
            signature = [System.IO.File]::ReadAllText($PortableSignaturePath).Trim()
            url = "https://github.com/PuppetWen/EnvNexus-AI/releases/download/$Tag/$PortableName"
            sha256 = $PortableHash
        }
    }
}
$Utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$LatestPath = Join-Path $ReleaseDirectory "latest.json"
[System.IO.File]::WriteAllText($LatestPath, ($Latest | ConvertTo-Json -Depth 5), $Utf8WithoutBom)

$HashTargets = @(
    $PortablePath,
    $PortableSignaturePath,
    $InstallerPath,
    $SignaturePath,
    $LatestPath
)
$HashLines = $HashTargets | ForEach-Object {
    $Hash = Get-FileHash -LiteralPath $_ -Algorithm SHA256
    "$($Hash.Hash.ToLowerInvariant())  $([System.IO.Path]::GetFileName($_))"
}
[System.IO.File]::WriteAllLines(
    (Join-Path $ReleaseDirectory "SHA256SUMS.txt"),
    $HashLines,
    $Utf8WithoutBom
)

$HashTargets + (Join-Path $ReleaseDirectory "SHA256SUMS.txt") |
    Get-Item |
    Select-Object Name, Length
