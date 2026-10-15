param([string]$Executable)

$ErrorActionPreference = 'Stop'
if ($env:CI -ne 'true') { throw 'Run this isolated installation check in CI.' }
$taskProject = Split-Path -Parent $PSScriptRoot
& (Join-Path $PSScriptRoot 'Use-ProjectCache.ps1')
if (-not $Executable) { $Executable = Join-Path $taskProject 'src-tauri/target/release/envnexus-ai.exe' }
$taskRun = Join-Path $taskProject ('artifacts/installability-' + [Guid]::NewGuid().ToString('N'))
$taskPreviousDataRoot = $env:ENVNEXUS_AI_DATA_ROOT
$env:ENVNEXUS_AI_DATA_ROOT = Join-Path $taskRun 'data'
New-Item -ItemType Directory -Path (Join-Path $env:ENVNEXUS_AI_DATA_ROOT 'cache') -Force | Out-Null
$taskIndex = @{ schemaVersion=1; scannedAt=[DateTime]::UtcNow.ToString('o'); executables=@{}; manifests=@() } | ConvertTo-Json
[IO.File]::WriteAllText((Join-Path $env:ENVNEXUS_AI_DATA_ROOT 'cache/tool-executable-discovery.json'), $taskIndex, [Text.UTF8Encoding]::new($false))
function Get-EnvironmentFingerprint {
    $taskVariables = [Environment]::GetEnvironmentVariables('User')
    $taskText = @($taskVariables.Keys | Sort-Object | ForEach-Object { "$_=$($taskVariables[$_])" }) -join "`n"
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($taskText)))
}
function Invoke-AppJson([string[]]$CommandArguments) {
    $taskOutput = & $Executable @CommandArguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw "Command failed ($($CommandArguments -join ' ')): $($taskOutput -join [Environment]::NewLine)" }
    ($taskOutput -join [Environment]::NewLine) | ConvertFrom-Json
}
$taskEnvironmentBefore = Get-EnvironmentFingerprint
$taskResults = @()
try {
    foreach ($taskTool in @('node','git')) {
        $taskToolRoot = Join-Path $taskRun ('tools/' + $taskTool)
        Invoke-AppJson @('root','set',$taskTool,$taskToolRoot,'--json') | Out-Null
        $taskCatalog = Invoke-AppJson @('versions',$taskTool,'--refresh','--json')
        $taskRemote = @($taskCatalog.versions | Where-Object { $_.channel -notmatch 'preview' })[0]
        if (-not $taskRemote) { throw "Official $taskTool catalog has no stable installation candidate" }
        $taskWatch = [Diagnostics.Stopwatch]::StartNew()
        $taskInstalled = Invoke-AppJson @('install',$taskTool,$taskRemote.version,'--yes','--json')
        $taskWatch.Stop()
        if ($taskInstalled.status -ne 'committed') { throw "$taskTool installation did not commit" }
        $taskManifest = Join-Path $taskInstalled.installationPath '.envpilot-install.json'
        if (-not (Test-Path -LiteralPath $taskManifest -PathType Leaf)) { throw "$taskTool installation manifest missing" }
        $taskPlan = Invoke-AppJson @('use',$taskTool,$taskInstalled.installationPath,'--json')
        if ($taskPlan.actionKind -ne 'switch' -or @($taskPlan.environmentDiffs).Count -eq 0) { throw "$taskTool activation preview is incomplete" }
        $taskCached = Invoke-AppJson @('versions',$taskTool,'--json')
        if (-not $taskCached.cached) { throw "$taskTool did not reuse its successful catalog" }
        $taskResults += [pscustomobject]@{ tool=$taskTool; version=$taskRemote.version; status=$taskInstalled.status; seconds=[Math]::Round($taskWatch.Elapsed.TotalSeconds,2); activationPreview='passed'; catalogCache='passed' }
    }
    if ($taskEnvironmentBefore -ne (Get-EnvironmentFingerprint)) { throw 'Installation or preview unexpectedly changed user environment variables' }
    [pscustomobject]@{ results=$taskResults; userEnvironment='unchanged' } | ConvertTo-Json -Depth 6 | Tee-Object -FilePath (Join-Path $taskRun 'summary.json')
} finally {
    $env:ENVNEXUS_AI_DATA_ROOT = $taskPreviousDataRoot
}
