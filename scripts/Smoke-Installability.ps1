param(
    [string]$Executable,
    [ValidateSet('python','java','go','rust','node','git','android-sdk','android-ndk','gradle','cmake','adb','maven','dotnet','ruby','php')]
    [string[]]$ToolIds = @('python','java','go','rust','node','git','android-sdk','android-ndk','gradle','cmake','adb','maven','dotnet','ruby','php')
)

$ErrorActionPreference = 'Stop'
if ($env:CI -ne 'true') { throw 'Run this isolated installation check in CI.' }
$taskProject = Split-Path -Parent $PSScriptRoot
& (Join-Path $PSScriptRoot 'Use-ProjectCache.ps1')
if (-not $Executable) { $Executable = Join-Path $taskProject 'src-tauri/target/release/envnexus-ai.exe' }
$Executable = [IO.Path]::GetFullPath($Executable)
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) { throw "Application executable not found: $Executable" }
if ($ToolIds.Count -eq 0 -or @($ToolIds | Select-Object -Unique).Count -ne $ToolIds.Count) { throw 'Select at least one unique tool ID.' }
$taskRun = [IO.Path]::GetFullPath((Join-Path $taskProject ('artifacts/installability-' + [Guid]::NewGuid().ToString('N'))))
if (Test-Path -LiteralPath $taskRun) { throw "Isolated run directory already exists: $taskRun" }
New-Item -ItemType Directory -Path $taskRun | Out-Null
$taskRun = (Resolve-Path -LiteralPath $taskRun).ProviderPath.TrimEnd('\','/')
$taskRunPrefix = $taskRun + [IO.Path]::DirectorySeparatorChar
$taskCommandIndex = 0
$taskToolReport = Join-Path $taskRun 'reports'
New-Item -ItemType Directory -Path $taskToolReport -Force | Out-Null
$taskComplete = $false
$taskEnvironmentNames = @(
    'ENVNEXUS_AI_DATA_ROOT','TEMP','TMP','GRADLE_USER_HOME','DOTNET_CLI_HOME',
    'DOTNET_SKIP_FIRST_TIME_EXPERIENCE','DOTNET_CLI_TELEMETRY_OPTOUT','DOTNET_NOLOGO',
    'NUGET_PACKAGES','ANDROID_USER_HOME','GOPATH','GOCACHE','GOMODCACHE'
)
$taskPreviousEnvironment = @{}
foreach ($taskName in $taskEnvironmentNames) {
    $taskPreviousEnvironment[$taskName] = [Environment]::GetEnvironmentVariable($taskName, 'Process')
}

function Get-EnvironmentFingerprint {
    $taskVariables = [Environment]::GetEnvironmentVariables('User')
    $taskText = @($taskVariables.Keys | Sort-Object | ForEach-Object { "$_=$($taskVariables[$_])" }) -join "`n"
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($taskText)))
}

function Get-NormalizedAbsolutePath([string]$Candidate) {
    if (-not $Candidate) { throw 'An empty isolated path is not allowed.' }
    if ($Candidate.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) {
        $Candidate = '\\' + $Candidate.Substring(8)
    } elseif ($Candidate.StartsWith('\\?\', [StringComparison]::OrdinalIgnoreCase)) {
        $Candidate = $Candidate.Substring(4)
    }
    [IO.Path]::GetFullPath($Candidate).TrimEnd('\','/')
}

function Assert-RunChildPath([string]$Candidate) {
    $taskAbsolute = Get-NormalizedAbsolutePath $Candidate
    if (-not $taskAbsolute.StartsWith($taskRunPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing a path outside this isolated run: $taskAbsolute"
    }
    # Resolve existing filesystem paths and reject junctions/symlinks in every
    # ancestor. Lexical containment alone does not protect a recursive deletion.
    $taskAncestor = $taskAbsolute
    while ($taskAncestor -and -not $taskAncestor.Equals($taskRun, [StringComparison]::OrdinalIgnoreCase)) {
        if (Test-Path -LiteralPath $taskAncestor) {
            $taskItem = Get-Item -LiteralPath $taskAncestor -Force
            if (($taskItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Refusing an isolated path through a reparse point: $taskAncestor" }
            $taskResolved = Get-NormalizedAbsolutePath (Resolve-Path -LiteralPath $taskAncestor).ProviderPath
            if (-not $taskResolved.StartsWith($taskRunPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw "Resolved path escapes this isolated run: $taskResolved" }
        }
        $taskAncestor = [IO.Path]::GetDirectoryName($taskAncestor)
    }
    $taskRootItem = Get-Item -LiteralPath $taskRun -Force
    if (($taskRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'The isolated run root became a reparse point.' }
    return $taskAbsolute
}

function Remove-RunDirectory([string]$Candidate) {
    $taskAbsolute = Assert-RunChildPath $Candidate
    if (-not (Test-Path -LiteralPath $taskAbsolute)) { return }
    # Do not follow directory links. Rustup creates file symbolic links for its
    # command proxies; unlink only those leaves after checking their real parent.
    $taskReparseChildren = @(Get-ChildItem -LiteralPath $taskAbsolute -Force -Recurse -ErrorAction Stop |
        Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 })
    foreach ($taskReparseChild in $taskReparseChildren) {
        if ($taskReparseChild.PSIsContainer -or
            ($taskReparseChild.Attributes -band [IO.FileAttributes]::Directory) -ne 0 -or
            $taskReparseChild.LinkType -cne 'SymbolicLink') {
            throw "Refusing recursive cleanup containing an unsupported reparse point: $($taskReparseChild.FullName)"
        }
    }
    foreach ($taskReparseChild in $taskReparseChildren) {
        $taskLinkPath = Get-NormalizedAbsolutePath $taskReparseChild.FullName
        if (-not $taskLinkPath.StartsWith($taskAbsolute + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing a symbolic link outside the cleanup directory: $taskLinkPath"
        }
        $taskLinkParent = Assert-RunChildPath ([IO.Path]::GetDirectoryName($taskLinkPath))
        $taskParentItem = Get-Item -LiteralPath $taskLinkParent -Force -ErrorAction Stop
        if (-not $taskParentItem.PSIsContainer) { throw "Symbolic link parent is not a directory: $taskLinkParent" }
        $taskResolvedParent = Get-NormalizedAbsolutePath (Resolve-Path -LiteralPath $taskLinkParent).ProviderPath
        if (-not $taskResolvedParent.StartsWith($taskRunPrefix, [StringComparison]::OrdinalIgnoreCase) -or
            -not $taskResolvedParent.Equals($taskLinkParent, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Symbolic link parent escapes the isolated run: $taskLinkParent"
        }
        # Re-read the leaf to reject a changed link type. Never resolve its
        # destination: Remove-Item without -Recurse removes the file link itself.
        $taskLinkItem = Get-Item -LiteralPath $taskLinkPath -Force -ErrorAction Stop
        if ($taskLinkItem.PSIsContainer -or
            ($taskLinkItem.Attributes -band [IO.FileAttributes]::Directory) -ne 0 -or
            ($taskLinkItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -or
            $taskLinkItem.LinkType -cne 'SymbolicLink') {
            throw "Symbolic link changed before cleanup: $taskLinkPath"
        }
        $null = Assert-RunChildPath $taskLinkParent
        Remove-Item -LiteralPath $taskLinkPath -Force -ErrorAction Stop
    }
    # Every reparse point must be gone before recursive directory removal.
    $taskUnsafeChild = Get-ChildItem -LiteralPath $taskAbsolute -Force -Recurse -ErrorAction Stop |
        Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 } |
        Select-Object -First 1
    if ($taskUnsafeChild) { throw "Refusing recursive cleanup containing a reparse point: $($taskUnsafeChild.FullName)" }
    # Revalidate the resolved absolute target immediately before deletion.
    $taskAbsolute = Assert-RunChildPath $taskAbsolute
    Remove-Item -LiteralPath $taskAbsolute -Recurse -Force -ErrorAction Stop
}

function Get-RunDirectoryBytes([string]$Candidate) {
    $taskAbsolute = Assert-RunChildPath $Candidate
    if (-not (Test-Path -LiteralPath $taskAbsolute)) { return 0L }
    [long](Get-ChildItem -LiteralPath $taskAbsolute -File -Recurse -Force -ErrorAction Stop | Measure-Object -Property Length -Sum).Sum
}

function Save-JsonReport([object]$Value, [string]$Path) {
    $taskReportPath = Assert-RunChildPath $Path
    [IO.File]::WriteAllText($taskReportPath, ($Value | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
}

function Invoke-AppJson([string[]]$CommandArguments) {
    $script:taskCommandIndex += 1
    $taskLog = Join-Path $taskToolReport ("command-{0:D2}.log" -f $script:taskCommandIndex)
    $taskOutput = & $Executable @CommandArguments 2>&1
    $taskExitCode = $LASTEXITCODE
    [IO.File]::WriteAllText($taskLog, ($taskOutput -join [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
    if ($taskExitCode -ne 0) { throw "Command failed ($($CommandArguments -join ' ')): $($taskOutput -join [Environment]::NewLine)" }
    ($taskOutput -join [Environment]::NewLine) | ConvertFrom-Json
}

$taskEnvironmentBefore = Get-EnvironmentFingerprint
$taskResults = @($ToolIds | ForEach-Object {
    [pscustomobject][ordered]@{
        tool=$_; version=$null; status='failed'; stage='not-started'; installationStatus=$null;
        seconds=0; installationPath=$null; manifest='not-checked'; activationPreview='not-checked';
        switchBlockedBySystemPath=$null; catalogCache='not-checked'; userEnvironment='not-checked';
        cachedDownloadBytes=0L; installedBytes=0L; cleanup='not-started'; error='The tool has not been attempted.'
    }
})

function Save-Summary {
    $taskUnchanged = $taskEnvironmentBefore -eq (Get-EnvironmentFingerprint)
    $taskSummary = [pscustomobject][ordered]@{
        artifactRoot=$taskRun; complete=$taskComplete; requestedTools=$ToolIds;
        toolCount=$taskResults.Count; passedCount=@($taskResults | Where-Object status -eq 'passed').Count;
        failedCount=@($taskResults | Where-Object status -ne 'passed').Count;
        userEnvironment=$(if ($taskUnchanged) { 'unchanged' } else { 'changed' }); results=$taskResults
    }
    Save-JsonReport $taskSummary (Join-Path $taskRun 'summary.json')
}

try {
    Save-Summary
    $taskDefinitions = @{}
    $taskDefinitionError = $null
    try {
        foreach ($taskDefinition in @(Invoke-AppJson @('tools','--json'))) { $taskDefinitions[[string]$taskDefinition.id] = $taskDefinition }
    } catch { $taskDefinitionError = $_.Exception.Message }

    foreach ($taskResult in $taskResults) {
        $taskTool = [string]$taskResult.tool
        $taskToolRoot = Join-Path $taskRun ('tools/' + $taskTool)
        $taskToolData = Join-Path $taskRun ('data/' + $taskTool)
        $taskToolTransient = Join-Path $taskRun ('temporary/' + $taskTool)
        $taskToolReport = Join-Path $taskRun ('reports/' + $taskTool)
        New-Item -ItemType Directory -Path $taskToolReport -Force | Out-Null
        $taskWatch = [Diagnostics.Stopwatch]::StartNew()
        Write-Host "Checking official installation and activation preview: $taskTool"
        try {
            $taskResult.error = $null
            $taskResult.stage = 'prepare'
            if ($taskDefinitionError) { throw $taskDefinitionError }
            $taskDefinition = $taskDefinitions[$taskTool]
            if (-not $taskDefinition) {
                throw "Tool is missing from the command engine's definitions: $taskTool"
            }
            foreach ($taskDirectory in @($taskToolRoot,$taskToolData,$taskToolTransient)) {
                $taskDirectory = Assert-RunChildPath $taskDirectory
                New-Item -ItemType Directory -Path $taskDirectory -Force | Out-Null
            }
            $env:ENVNEXUS_AI_DATA_ROOT = $taskToolData
            $taskCacheVariables = [ordered]@{
                TEMP='tmp'; TMP='tmp'; GRADLE_USER_HOME='gradle'; DOTNET_CLI_HOME='dotnet';
                NUGET_PACKAGES='nuget'; ANDROID_USER_HOME='android'; GOPATH='go'; GOCACHE='go-cache'; GOMODCACHE='go-mod'
            }
            foreach ($taskName in $taskCacheVariables.Keys) {
                $taskCachePath = Assert-RunChildPath (Join-Path $taskToolTransient $taskCacheVariables[$taskName])
                New-Item -ItemType Directory -Path $taskCachePath -Force | Out-Null
                [Environment]::SetEnvironmentVariable($taskName, $taskCachePath, 'Process')
            }
            $env:DOTNET_SKIP_FIRST_TIME_EXPERIENCE = '1'
            $env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
            $env:DOTNET_NOLOGO = '1'
            New-Item -ItemType Directory -Path (Join-Path $taskToolData 'cache') -Force | Out-Null
            $taskIndex = @{ schemaVersion=1; scannedAt=[DateTime]::UtcNow.ToString('o'); executables=@{}; manifests=@() }
            Save-JsonReport $taskIndex (Join-Path $taskToolData 'cache/tool-executable-discovery.json')
            Invoke-AppJson @('root','set',$taskTool,$taskToolRoot,'--json') | Out-Null
            $taskResult.stage = 'catalog'
            $taskCatalog = Invoke-AppJson @('versions',$taskTool,'--refresh','--json')
            Save-JsonReport $taskCatalog (Join-Path $taskToolReport 'catalog.json')
            # The command engine returns versions in numeric descending order.
            # Stable includes LTS/current and excludes prerelease channel/version labels.
            $taskRemote = @($taskCatalog.versions | Where-Object {
                $_.channel -notmatch '(?i)preview|alpha|beta|nightly|snapshot|early.?access|(^|[\s._-])rc([\s._-]|\d|$)' -and
                $_.version -notmatch '(?i)(?:\d|[._-])(?:a|alpha|b|beta|rc|pre|preview|dev|snapshot|nightly|ea)(?:\d|[._-]|$)'
            }) | Select-Object -First 1
            if (-not $taskRemote) { throw "Official $taskTool catalog has no stable installation candidate." }
            if (-not $taskRemote.downloadUrl) { throw "The highest stable $taskTool version has no download URL." }
            $taskResult.version = [string]$taskRemote.version
            $taskResult.stage = 'install'
            # Python follows the production catalog and normal CLI install path.
            # That catalog currently selects an embeddable ZIP, not the full CPython installer.
            $taskInstalled = Invoke-AppJson @('install',$taskTool,$taskRemote.version,'--yes','--json')
            Save-JsonReport $taskInstalled (Join-Path $taskToolReport 'installation.json')
            $taskResult.installationStatus = [string]$taskInstalled.status
            if ($taskInstalled.status -ne 'committed') { throw "$taskTool installation did not commit." }
            $taskInstallation = Assert-RunChildPath ([string]$taskInstalled.installationPath)
            $taskToolRootAbsolute = Get-NormalizedAbsolutePath $taskToolRoot
            if (-not $taskInstallation.StartsWith($taskToolRootAbsolute + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
                throw "$taskTool installation escaped its selected tool root."
            }
            $taskResult.installationPath = $taskInstallation
            $taskResult.stage = 'manifest'
            $taskManifestPath = Join-Path $taskInstallation '.envpilot-install.json'
            if (-not (Test-Path -LiteralPath $taskManifestPath -PathType Leaf)) { throw "$taskTool installation manifest missing." }
            $taskManifest = Get-Content -LiteralPath $taskManifestPath -Raw | ConvertFrom-Json
            Save-JsonReport $taskManifest (Join-Path $taskToolReport 'install-manifest.json')
            if ($taskManifest.toolId -ne $taskTool -or $taskManifest.version -ne $taskRemote.version -or
                (Get-NormalizedAbsolutePath ([string]$taskManifest.installationPath)) -ine $taskInstallation -or
                (Get-NormalizedAbsolutePath ([string]$taskManifest.managedRoot)) -ine $taskToolRootAbsolute) {
                throw "$taskTool installation manifest does not match the requested version and root."
            }
            $taskResult.manifest = 'passed'
            $taskResult.stage = 'activation-preview'
            $taskPlan = Invoke-AppJson @('use',$taskTool,$taskInstallation,'--json')
            Save-JsonReport $taskPlan (Join-Path $taskToolReport 'activation-preview.json')
            if ($taskPlan.actionKind -ne 'switch' -or @($taskPlan.environmentDiffs).Count -eq 0) { throw "$taskTool activation preview is incomplete." }
            $taskResult.activationPreview = 'passed'
            $taskResult.switchBlockedBySystemPath = [bool]$taskPlan.requiresElevation
            $taskResult.stage = 'catalog-cache'
            $taskCached = Invoke-AppJson @('versions',$taskTool,'--json')
            if (-not $taskCached.cached) { throw "$taskTool did not reuse its successful catalog." }
            $taskResult.catalogCache = 'passed'
            $taskResult.stage = 'complete'
            $taskResult.status = 'passed'
        } catch {
            $taskResult.status = 'failed'
            $taskResult.error = $_.Exception.Message
            Write-Warning "$taskTool failed during $($taskResult.stage): $($taskResult.error)"
        } finally {
            $taskWatch.Stop()
            $taskResult.seconds = [Math]::Round($taskWatch.Elapsed.TotalSeconds,2)
            if ($taskEnvironmentBefore -eq (Get-EnvironmentFingerprint)) {
                $taskResult.userEnvironment = 'unchanged'
            } else {
                $taskResult.userEnvironment = 'changed'
                $taskResult.status = 'failed'
                $taskResult.error = @($taskResult.error,'Installation or preview unexpectedly changed user environment variables.') | Where-Object { $_ } | Join-String -Separator ' | '
            }
            $taskCleanupErrors = @()
            try {
                $taskResult.cachedDownloadBytes = Get-RunDirectoryBytes (Join-Path $taskToolData 'downloads')
                $taskResult.installedBytes = Get-RunDirectoryBytes $taskToolRoot
            } catch { $taskCleanupErrors += $_.Exception.Message }
            # Preserve small manifests/plans/logs; remove only this tool's large
            # installation, downloaded archives, and first-run temporary caches.
            foreach ($taskCleanupPath in @($taskToolRoot,(Join-Path $taskToolData 'downloads'),$taskToolTransient)) {
                try { Remove-RunDirectory $taskCleanupPath } catch { $taskCleanupErrors += $_.Exception.Message }
            }
            if ($taskCleanupErrors.Count -gt 0) {
                $taskResult.cleanup = 'failed'
                $taskResult.status = 'failed'
                $taskResult.error = @($taskResult.error) + $taskCleanupErrors | Where-Object { $_ } | Join-String -Separator ' | '
            } else { $taskResult.cleanup = 'passed' }
            foreach ($taskName in $taskEnvironmentNames) {
                [Environment]::SetEnvironmentVariable($taskName, $taskPreviousEnvironment[$taskName], 'Process')
            }
            Save-Summary
        }
    }
    $taskComplete = $true
    Save-Summary
    Get-Content -LiteralPath (Join-Path $taskRun 'summary.json') -Raw | Write-Output
    $taskFailures = @($taskResults | Where-Object status -ne 'passed')
    if ($taskFailures.Count -gt 0 -or $taskEnvironmentBefore -ne (Get-EnvironmentFingerprint)) {
        throw "$($taskFailures.Count) of $($taskResults.Count) official installation checks failed. See $taskRun\summary.json."
    }
} finally {
    foreach ($taskName in $taskEnvironmentNames) {
        [Environment]::SetEnvironmentVariable($taskName, $taskPreviousEnvironment[$taskName], 'Process')
    }
}
