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

Copy-Item -LiteralPath $ExecutableSource -Destination $PortablePath -Force
Copy-Item -LiteralPath $InstallerSource -Destination $InstallerPath -Force
Copy-Item -LiteralPath $SignatureSource -Destination $SignaturePath -Force

$Latest = [ordered]@{
    version = $Version
    notes = $ReleaseNotes
    pub_date = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = [ordered]@{
        "windows-x86_64" = [ordered]@{
            signature = [System.IO.File]::ReadAllText($SignatureSource).Trim()
            url = "https://github.com/PuppetWen/EnvNexus-AI/releases/download/$Tag/$InstallerName"
        }
    }
}
$Utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$LatestPath = Join-Path $ReleaseDirectory "latest.json"
[System.IO.File]::WriteAllText($LatestPath, ($Latest | ConvertTo-Json -Depth 5), $Utf8WithoutBom)

$HashTargets = @($PortablePath, $InstallerPath, $SignaturePath, $LatestPath)
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
