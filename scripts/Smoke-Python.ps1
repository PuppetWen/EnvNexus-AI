param([string]$Executable)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($env:CI -ne 'true' -or $env:GITHUB_ACTIONS -ne 'true' -or -not $IsWindows) {
    throw 'Run this isolated Python installation check only on the Windows GitHub CI runner.'
}
$taskProject = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd('\','/')
if (-not $taskProject.Equals([IO.Path]::GetFullPath($env:GITHUB_WORKSPACE).TrimEnd('\','/'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The Python check must run from the checked-out GitHub workspace.'
}
if ([IO.Path]::GetPathRoot($taskProject) -notin @('D:\','E:\')) {
    throw 'The isolated Python installation, caches, and temporary files must be on D: or E:.'
}
if (-not $Executable) { $Executable = Join-Path $taskProject 'src-tauri/target/release/envnexus-ai.exe' }
$Executable = [IO.Path]::GetFullPath($Executable)
$taskRunParent = Join-Path $taskProject 'artifacts'
$taskAncestor = $taskRunParent
while ($taskAncestor) {
    if ((Test-Path -LiteralPath $taskAncestor) -and
        ((Get-Item -LiteralPath $taskAncestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing an artifact directory through a reparse point: $taskAncestor"
    }
    $taskAncestor = [IO.Path]::GetDirectoryName($taskAncestor)
}
$taskRun = [IO.Path]::GetFullPath((Join-Path $taskRunParent ('python-' + [Guid]::NewGuid().ToString('N'))))
if (Test-Path -LiteralPath $taskRun) { throw "The isolated run already exists: $taskRun" }
New-Item -ItemType Directory -Path $taskRun | Out-Null
$taskRun = (Resolve-Path -LiteralPath $taskRun).ProviderPath.TrimEnd('\','/')
$taskRunPrefix = $taskRun + [IO.Path]::DirectorySeparatorChar
$taskReports = Join-Path $taskRun 'reports'
New-Item -ItemType Directory -Path $taskReports | Out-Null
$taskCommandIndex = 0
# Pin both cases to the same public release so repair exercises a distribution
# migration rather than accidentally succeeding by upgrading the Python version.
$taskVersion = '3.15.0'
$taskFullUrl = "https://www.python.org/ftp/python/$taskVersion/python-$taskVersion-amd64.zip"
$taskEmbeddedUrl = 'https://www.python.org/ftp/python/3.15.0/python-3.15.0-embed-amd64.zip'
$taskEmbeddedHash = 'a6999bcbcbe56f937159ef79c2630260eca99f65f7860bc5cd20430324079a44'
$taskEnvironmentNames = @(
    'ENVNEXUS_AI_DATA_ROOT','TEMP','TMP','PIP_CACHE_DIR','PIP_CONFIG_FILE',
    'PIP_NO_INDEX','PIP_DISABLE_PIP_VERSION_CHECK','PYTHONUSERBASE','PYTHONPYCACHEPREFIX',
    'PYTHONHOME','PYTHONPATH','PYTHONSTARTUP','XDG_CACHE_HOME','UV_CACHE_DIR'
)
$taskPreviousEnvironment = @{}
foreach ($taskName in $taskEnvironmentNames) {
    $taskPreviousEnvironment[$taskName] = [Environment]::GetEnvironmentVariable($taskName, 'Process')
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
    $taskAncestor = $taskAbsolute
    while ($taskAncestor -and -not $taskAncestor.Equals($taskRun, [StringComparison]::OrdinalIgnoreCase)) {
        if (Test-Path -LiteralPath $taskAncestor) {
            $taskItem = Get-Item -LiteralPath $taskAncestor -Force
            if (($taskItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Refusing a path through a reparse point: $taskAncestor"
            }
            $taskResolved = Get-NormalizedAbsolutePath (Resolve-Path -LiteralPath $taskAncestor).ProviderPath
            if (-not $taskResolved.StartsWith($taskRunPrefix, [StringComparison]::OrdinalIgnoreCase)) {
                throw "Resolved path escapes the isolated run: $taskResolved"
            }
        }
        $taskAncestor = [IO.Path]::GetDirectoryName($taskAncestor)
    }
    if (((Get-Item -LiteralPath $taskRun -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'The isolated run root became a reparse point.'
    }
    $taskAbsolute
}

function New-RunDirectory([string]$Candidate) {
    $taskAbsolute = Assert-RunChildPath $Candidate
    New-Item -ItemType Directory -Path $taskAbsolute -Force | Out-Null
    $null = Assert-RunChildPath $taskAbsolute
}

function Remove-RunDirectory([string]$Candidate) {
    $taskAbsolute = Assert-RunChildPath $Candidate
    if (-not (Test-Path -LiteralPath $taskAbsolute)) { return }
    # Windows venv uses copies by default. Refuse every link rather than follow
    # an unexpected junction or symlink while removing this run's large files.
    $taskLink = Get-ChildItem -LiteralPath $taskAbsolute -Force -Recurse -ErrorAction Stop |
        Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 } |
        Select-Object -First 1
    if ($taskLink) { throw "Refusing recursive cleanup containing a reparse point: $($taskLink.FullName)" }
    $taskAbsolute = Assert-RunChildPath $taskAbsolute
    Remove-Item -LiteralPath $taskAbsolute -Recurse -Force -ErrorAction Stop
}

function Save-JsonReport([object]$Value, [string]$Path) {
    $taskPath = Assert-RunChildPath $Path
    [IO.File]::WriteAllText($taskPath, (ConvertTo-Json -InputObject $Value -Depth 15), [Text.UTF8Encoding]::new($false))
}

function Get-UserEnvironmentFingerprint {
    $taskKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
    $taskValues = @()
    if ($taskKey) {
        try {
            $taskValues = @($taskKey.GetValueNames() | Sort-Object | ForEach-Object {
                [ordered]@{
                    name=$_; kind=$taskKey.GetValueKind($_).ToString()
                    value=$taskKey.GetValue($_, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
                }
            })
        } finally { $taskKey.Dispose() }
    }
    $taskJson = ConvertTo-Json -InputObject ([ordered]@{ keyExists=($null -ne $taskKey); values=$taskValues }) -Depth 8 -Compress
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($taskJson))).ToLowerInvariant()
}

function Invoke-CheckedCommand([string]$Program, [string[]]$CommandArguments, [string]$Label) {
    $script:taskCommandIndex += 1
    $taskLog = Assert-RunChildPath (Join-Path $taskReports ('{0:D2}-{1}.json' -f $script:taskCommandIndex,$Label))
    $taskWatch = [Diagnostics.Stopwatch]::StartNew()
    $taskExit = -1
    try {
        $taskOutput = & $Program @CommandArguments 2>&1
        $taskExit = $LASTEXITCODE
    } catch {
        $taskOutput = @($_.Exception.Message)
    }
    $taskWatch.Stop()
    $taskText = $taskOutput -join [Environment]::NewLine
    Save-JsonReport ([ordered]@{
        program=$Program; arguments=$CommandArguments; exitCode=$taskExit
        seconds=[Math]::Round($taskWatch.Elapsed.TotalSeconds,2); output=$taskText
    }) $taskLog
    if ($taskExit -ne 0) { throw "Python smoke command failed ($Label, exit $taskExit): $taskText" }
    $taskText
}

function Invoke-AppJson([string[]]$CommandArguments, [string]$Label) {
    $taskText = Invoke-CheckedCommand -Program $Executable -CommandArguments $CommandArguments -Label $Label
    $taskText | ConvertFrom-Json -NoEnumerate
}

function Set-ScenarioEnvironment([string]$Scenario) {
    $taskScenario = Assert-RunChildPath (Join-Path $taskRun $Scenario)
    foreach ($taskChild in @('tools','data','temporary','temporary/tmp','temporary/pip-cache','temporary/user-base','temporary/pycache','temporary/xdg','temporary/uv')) {
        New-RunDirectory (Join-Path $taskScenario $taskChild)
    }
    $taskSettings = [ordered]@{
        ENVNEXUS_AI_DATA_ROOT=(Join-Path $taskScenario 'data')
        TEMP=(Join-Path $taskScenario 'temporary/tmp'); TMP=(Join-Path $taskScenario 'temporary/tmp')
        PIP_CACHE_DIR=(Join-Path $taskScenario 'temporary/pip-cache')
        PIP_CONFIG_FILE=(Join-Path $taskScenario 'temporary/pip.ini')
        PIP_NO_INDEX='1'; PIP_DISABLE_PIP_VERSION_CHECK='1'
        PYTHONUSERBASE=(Join-Path $taskScenario 'temporary/user-base')
        PYTHONPYCACHEPREFIX=(Join-Path $taskScenario 'temporary/pycache')
        PYTHONHOME=$null; PYTHONPATH=$null; PYTHONSTARTUP=$null
        XDG_CACHE_HOME=(Join-Path $taskScenario 'temporary/xdg'); UV_CACHE_DIR=(Join-Path $taskScenario 'temporary/uv')
    }
    foreach ($taskName in $taskSettings.Keys) {
        [Environment]::SetEnvironmentVariable($taskName, $taskSettings[$taskName], 'Process')
    }
    # An explicit empty index keeps this test focused on the selected directory.
    New-RunDirectory (Join-Path $taskScenario 'data/cache')
    Save-JsonReport ([ordered]@{ schemaVersion=1; scannedAt=[DateTime]::UtcNow.ToString('o'); executables=@{}; manifests=@() }) (Join-Path $taskScenario 'data/cache/tool-executable-discovery.json')
    $taskScenario
}

function Assert-FullRemote([object]$Remote) {
    if (-not $Remote -or $Remote.version -cne $taskVersion -or
        $Remote.downloadUrl -cne $taskFullUrl -or $Remote.architecture -cne 'x86_64' -or
        $Remote.checksumAlgorithm -ine 'sha256' -or $Remote.checksum -notmatch '^[a-fA-F0-9]{64}$') {
        throw 'The production catalog did not select the SHA-256 verified official full Python ZIP.'
    }
}

function Assert-Manifest([string]$Installation, [string]$ManagedRoot, [string]$DataRoot, [object]$Remote, [string]$Label) {
    $taskManifest = Get-Content -LiteralPath (Assert-RunChildPath (Join-Path $Installation '.envpilot-install.json')) -Raw | ConvertFrom-Json
    if ($taskManifest.toolId -cne 'python' -or $taskManifest.version -cne $taskVersion -or
        $taskManifest.sourceUrl -cne $taskFullUrl -or $taskManifest.checksumAlgorithm -ine 'sha256' -or
        $taskManifest.checksum -ine $Remote.checksum -or
        (Get-NormalizedAbsolutePath $taskManifest.installationPath) -ine $Installation -or
        (Get-NormalizedAbsolutePath $taskManifest.managedRoot) -ine $ManagedRoot) {
        throw "$Label manifest does not describe the selected full Python runtime and isolated destination."
    }
    $taskCentral = Assert-RunChildPath (Join-Path $DataRoot ('config/installations/' + $taskManifest.operationId + '.json'))
    if (-not (Test-Path -LiteralPath $taskCentral -PathType Leaf)) { throw "$Label central manifest is missing." }
    $taskCentralManifest = Get-Content -LiteralPath $taskCentral -Raw | ConvertFrom-Json
    if ($taskCentralManifest.sourceUrl -cne $taskFullUrl -or $taskCentralManifest.operationId -cne $taskManifest.operationId) {
        throw "$Label central manifest does not match the installed runtime."
    }
    $taskArchive = Assert-RunChildPath (Join-Path $DataRoot "downloads/python/python-$taskVersion-amd64.zip")
    $taskArchiveHash = (Get-FileHash -LiteralPath $taskArchive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($taskArchiveHash -ine $Remote.checksum) { throw "$Label downloaded ZIP does not match the official full-package SHA-256." }
    Save-JsonReport ([ordered]@{ sourceUrl=$taskFullUrl; sha256=$taskArchiveHash; bytes=(Get-Item -LiteralPath $taskArchive).Length }) (Join-Path $taskReports "$Label-download.json")
    Save-JsonReport $taskManifest (Join-Path $taskReports "$Label-manifest.json")
    $taskManifest
}

function Test-DevelopmentRuntime([string]$Installation, [string]$Scenario, [string]$Label) {
    $taskPython = Assert-RunChildPath (Join-Path $Installation 'python.exe')
    $taskPip = Assert-RunChildPath (Join-Path $Installation 'Scripts/pip.exe')
    foreach ($taskRelative in @('Lib/venv/__init__.py','Lib/ensurepip/__init__.py','Lib/tkinter/__init__.py','Lib/idlelib/__init__.py','include/Python.h','Scripts/pip.exe')) {
        if (-not (Test-Path -LiteralPath (Assert-RunChildPath (Join-Path $Installation $taskRelative)) -PathType Leaf)) {
            throw "$Label is missing a normal development component: $taskRelative"
        }
    }
    if (@(Get-ChildItem -LiteralPath $Installation -Filter 'python*._pth').Count -ne 0) { throw "$Label retained an embedded distribution marker." }
    $taskLibraries = @(Get-ChildItem -LiteralPath (Join-Path $Installation 'libs') -Filter '*.lib' -File)
    if ($taskLibraries.Count -eq 0) { throw "$Label has no Python import libraries." }
    $taskProbe = @'
import ensurepip, idlelib, json, pathlib, pip, sys, tkinter, venv
root = pathlib.Path(sys.argv[1]).resolve()
assert pathlib.Path(sys.prefix).resolve() == root
assert root in pathlib.Path(pip.__file__).resolve().parents
assert '.'.join(map(str, sys.version_info[:3])) == sys.argv[2]
print(json.dumps({'version': sys.version.split()[0], 'prefix': sys.prefix, 'pipVersion': pip.__version__, 'pipModule': pip.__file__, 'tkVersion': tkinter.TkVersion, 'idleModule': idlelib.__file__}))
'@
    $taskRuntime = (Invoke-CheckedCommand -Program $taskPython -CommandArguments @('-I','-c',$taskProbe,$Installation,$taskVersion) -Label "$Label-runtime") | ConvertFrom-Json
    $taskPipList = @((Invoke-CheckedCommand -Program $taskPython -CommandArguments @('-I','-m','pip','--isolated','--disable-pip-version-check','--no-cache-dir','list','--format=json') -Label "$Label-pip-list") | ConvertFrom-Json)
    if (-not @($taskPipList | Where-Object name -ieq 'pip')) { throw "$Label pip list did not contain pip." }
    $taskLauncherList = @((Invoke-CheckedCommand -Program $taskPip -CommandArguments @('--isolated','--disable-pip-version-check','--no-cache-dir','list','--format=json') -Label "$Label-pip-exe-list") | ConvertFrom-Json)
    if (-not @($taskLauncherList | Where-Object name -ieq 'pip')) { throw "$Label pip.exe did not run pip." }
    $taskLauncherVersion = Invoke-CheckedCommand -Program $taskPip -CommandArguments @('--isolated','--disable-pip-version-check','--version') -Label "$Label-pip-exe-version"
    if (-not $taskLauncherVersion.ToLowerInvariant().Contains($Installation.ToLowerInvariant())) { throw "$Label pip.exe points outside the final installation." }

    $taskVenv = Assert-RunChildPath (Join-Path $Scenario 'venv')
    $null = Invoke-CheckedCommand -Program $taskPython -CommandArguments @('-I','-m','venv',$taskVenv) -Label "$Label-create-venv"
    $taskVenvPython = Assert-RunChildPath (Join-Path $taskVenv 'Scripts/python.exe')
    $taskVenvPip = Assert-RunChildPath (Join-Path $taskVenv 'Scripts/pip.exe')
    $taskVenvProbe = @'
import json, pathlib, pip, statistics, sys
env, root = map(lambda value: pathlib.Path(value).resolve(), sys.argv[1:3])
assert pathlib.Path(sys.prefix).resolve() == env
assert pathlib.Path(sys.base_prefix).resolve() == root
assert env in pathlib.Path(pip.__file__).resolve().parents
assert sys.prefix != sys.base_prefix
assert statistics.mean([2, 4, 6]) == 4
print(json.dumps({'prefix': sys.prefix, 'basePrefix': sys.base_prefix, 'pipVersion': pip.__version__, 'pipModule': pip.__file__, 'programResult': 4}))
'@
    $taskVenvRuntime = (Invoke-CheckedCommand -Program $taskVenvPython -CommandArguments @('-I','-c',$taskVenvProbe,$taskVenv,$Installation) -Label "$Label-venv-runtime") | ConvertFrom-Json
    $taskVenvPackages = @((Invoke-CheckedCommand -Program $taskVenvPython -CommandArguments @('-I','-m','pip','--isolated','--disable-pip-version-check','--no-cache-dir','list','--format=json') -Label "$Label-venv-pip-list") | ConvertFrom-Json)
    if (-not @($taskVenvPackages | Where-Object name -ieq 'pip')) { throw "$Label venv did not receive pip." }
    $taskVenvLauncher = Invoke-CheckedCommand -Program $taskVenvPip -CommandArguments @('--isolated','--disable-pip-version-check','--version') -Label "$Label-venv-pip-exe"
    if (-not $taskVenvLauncher.ToLowerInvariant().Contains($taskVenv.ToLowerInvariant())) { throw "$Label venv pip.exe points outside the venv." }
    $taskChecks = [ordered]@{
        status='passed'; runtime=$taskRuntime; pipList=$taskPipList; pipExeList=$taskLauncherList
        pipExeVersion=$taskLauncherVersion; includeHeader='include/Python.h'; importLibraryCount=$taskLibraries.Count
        venv=$taskVenvRuntime; venvPipList=$taskVenvPackages; venvPipExeVersion=$taskVenvLauncher
    }
    Save-JsonReport $taskChecks (Join-Path $taskReports "$Label-development.json")
    $taskChecks
}

$taskSummary = [ordered]@{
    artifactRoot=$taskRun; executable=$Executable; version=$taskVersion; startedAt=[DateTime]::UtcNow.ToString('o'); finishedAt=$null
    status='running'; complete=$false; userEnvironment='not-checked'; userEnvironmentBefore=$null; userEnvironmentAfter=$null
    fresh=[ordered]@{ status='not-started'; stage='prepare'; sourceUrl=$null; installationPath=$null; runtime=$null; userEnvironment='not-checked'; error=$null }
    repair=[ordered]@{ status='not-started'; stage='prepare'; sourceUrlBefore=$taskEmbeddedUrl; sourceUrlAfter=$null; installationPath=$null; sameVersion=$false; oldCatalogRejected=$false; runtime=$null; userEnvironment='not-checked'; error=$null }
    cleanup='not-started'; errors=@()
}
try {
    Save-JsonReport $taskSummary (Join-Path $taskRun 'summary.json')
    $taskSummary.userEnvironmentBefore = Get-UserEnvironmentFingerprint
    if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) { throw "Application executable not found: $Executable" }

    try {
        $taskFresh = Set-ScenarioEnvironment 'fresh'
        $taskFreshRoot = Assert-RunChildPath (Join-Path $taskFresh 'tools')
        $null = Invoke-AppJson -CommandArguments @('root','set','python',$taskFreshRoot,'--json') -Label 'fresh-root'
        $taskSummary.fresh.stage = 'official-catalog'
        $taskCatalog = Invoke-AppJson -CommandArguments @('versions','python','--refresh','--json') -Label 'fresh-catalog'
        Save-JsonReport $taskCatalog (Join-Path $taskReports 'fresh-catalog.json')
        $taskRemote = @($taskCatalog.versions | Where-Object version -ceq $taskVersion) | Select-Object -First 1
        Assert-FullRemote $taskRemote
        $taskSummary.fresh.sourceUrl = $taskRemote.downloadUrl
        $taskSummary.fresh.stage = 'install'
        $taskInstalled = Invoke-AppJson -CommandArguments @('install','python',$taskVersion,'--yes','--json') -Label 'fresh-install'
        Save-JsonReport $taskInstalled (Join-Path $taskReports 'fresh-installation.json')
        if ($taskInstalled.status -cne 'committed') { throw 'Fresh Python installation did not commit.' }
        $taskInstallation = Assert-RunChildPath $taskInstalled.installationPath
        $taskExpected = Get-NormalizedAbsolutePath (Join-Path $taskFreshRoot "python/$taskVersion")
        if ($taskInstallation -ine $taskExpected) { throw 'Fresh Python escaped the requested version directory.' }
        $taskSummary.fresh.installationPath = $taskInstallation
        $taskSummary.fresh.stage = 'manifest'
        $null = Assert-Manifest $taskInstallation $taskFreshRoot (Join-Path $taskFresh 'data') $taskRemote 'fresh'
        $taskSummary.fresh.stage = 'development-runtime'
        $taskSummary.fresh.runtime = Test-DevelopmentRuntime $taskInstallation $taskFresh 'fresh'
        $taskSummary.fresh.stage = 'complete'
        $taskSummary.fresh.status = 'passed'
    } catch {
        $taskSummary.fresh.status = 'failed'
        $taskSummary.fresh.error = $_.Exception.Message
        $taskSummary.errors += "Fresh: $($_.Exception.Message)"
    } finally {
        try {
            $taskSummary.fresh.userEnvironment = if ((Get-UserEnvironmentFingerprint) -ceq $taskSummary.userEnvironmentBefore) { 'unchanged' } else { 'changed' }
            if ($taskSummary.fresh.userEnvironment -cne 'unchanged') { throw 'Fresh installation unexpectedly changed the user environment.' }
        } catch {
            $taskSummary.fresh.status = 'failed'
            $taskSummary.fresh.error = @($taskSummary.fresh.error,$_.Exception.Message) | Where-Object { $_ } | Join-String -Separator ' | '
            $taskSummary.errors += $_.Exception.Message
        }
    }
    Save-JsonReport $taskSummary (Join-Path $taskRun 'summary.json')

    try {
        $taskLegacy = Set-ScenarioEnvironment 'legacy'
        $taskLegacyRoot = Assert-RunChildPath (Join-Path $taskLegacy 'tools')
        $taskLegacyData = Assert-RunChildPath (Join-Path $taskLegacy 'data')
        $taskLegacyInstallation = Assert-RunChildPath (Join-Path $taskLegacyRoot "python/$taskVersion")
        $null = Invoke-AppJson -CommandArguments @('root','set','python',$taskLegacyRoot,'--json') -Label 'legacy-root'
        $taskSummary.repair.stage = 'embedded-fixture'
        New-RunDirectory (Join-Path $taskLegacyData 'downloads/python')
        $taskFixture = Assert-RunChildPath (Join-Path $taskLegacyData 'downloads/python/python-3.15.0-embed-amd64.zip')
        Invoke-WebRequest -Uri $taskEmbeddedUrl -OutFile $taskFixture -TimeoutSec 180 -MaximumRetryCount 2 -RetryIntervalSec 2
        $taskActualHash = (Get-FileHash -LiteralPath $taskFixture -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($taskActualHash -cne $taskEmbeddedHash) { throw 'The old embedded Python fixture failed its pinned SHA-256 check.' }
        New-RunDirectory $taskLegacyInstallation
        Expand-Archive -LiteralPath $taskFixture -DestinationPath $taskLegacyInstallation
        if (@(Get-ChildItem -LiteralPath $taskLegacyInstallation -Filter 'python*._pth').Count -ne 1 -or
            (Test-Path -LiteralPath (Join-Path $taskLegacyInstallation 'Scripts/pip.exe')) -or
            (Test-Path -LiteralPath (Join-Path $taskLegacyInstallation 'Lib/venv/__init__.py'))) {
            throw 'The old embedded fixture does not reproduce the missing pip/venv development environment.'
        }
        $taskLegacyId = [Guid]::NewGuid().ToString()
        $taskOldManifest = [ordered]@{
            schemaVersion=1; operationId=$taskLegacyId; toolId='python'; version=$taskVersion; installedAt=[DateTime]::UtcNow.ToString('o')
            managedRoot=$taskLegacyRoot; installationPath=$taskLegacyInstallation; sourceUrl=$taskEmbeddedUrl
            checksumAlgorithm='sha256'; checksum=$taskEmbeddedHash
        }
        New-RunDirectory (Join-Path $taskLegacyData 'config/installations')
        Save-JsonReport $taskOldManifest (Join-Path $taskLegacyInstallation '.envpilot-install.json')
        $taskOldCentral = Join-Path $taskLegacyData "config/installations/$taskLegacyId.json"
        Save-JsonReport $taskOldManifest $taskOldCentral
        Save-JsonReport $taskOldManifest (Join-Path $taskReports 'legacy-manifest-before.json')
        $taskOldCatalog = [ordered]@{
            toolId='python'; sourceName='Python.org (embedded legacy fixture)'; sourceUrl='https://www.python.org/api/v2/downloads/release/'
            fetchedAt=[DateTime]::UtcNow.ToString('o'); cached=$false
            versions=@([ordered]@{ version=$taskVersion; channel='stable'; publishedAt=$null; architecture='x86_64'; downloadUrl=$taskEmbeddedUrl; checksumAlgorithm='sha256'; checksum=$taskEmbeddedHash; notesUrl=$null })
        }
        New-RunDirectory (Join-Path $taskLegacyData 'cache/version-sources')
        $taskLegacyCatalogPath = Join-Path $taskLegacyData 'cache/version-sources/python.json'
        Save-JsonReport $taskOldCatalog $taskLegacyCatalogPath
        Save-JsonReport $taskOldCatalog (Join-Path $taskReports 'legacy-catalog-before.json')
        Save-JsonReport ([ordered]@{ sourceUrl=$taskEmbeddedUrl; sha256=$taskActualHash; bytes=(Get-Item -LiteralPath $taskFixture).Length; embeddedMarker=$true; pipExePresent=$false; venvPresent=$false }) (Join-Path $taskReports 'legacy-fixture.json')

        # Do not refresh the catalog first: production CLI repair itself must
        # reject the fresh old cache and select the full ZIP for this same version.
        $taskSummary.repair.stage = 'repair-with-old-catalog'
        $taskRepaired = Invoke-AppJson -CommandArguments @('repair','python',$taskLegacyInstallation,'--yes','--json') -Label 'legacy-repair'
        Save-JsonReport $taskRepaired (Join-Path $taskReports 'legacy-repair.json')
        if ($taskRepaired.status -cne 'committed' -or (Get-NormalizedAbsolutePath $taskRepaired.installationPath) -ine $taskLegacyInstallation) {
            throw 'The same-version Python repair did not commit at the existing installation path.'
        }
        $taskSummary.repair.installationPath = $taskLegacyInstallation
        $taskSummary.repair.stage = 'catalog-migration'
        $taskNewCatalog = Get-Content -LiteralPath $taskLegacyCatalogPath -Raw | ConvertFrom-Json
        $taskRepairRemote = @($taskNewCatalog.versions | Where-Object version -ceq $taskVersion) | Select-Object -First 1
        Assert-FullRemote $taskRepairRemote
        if ($taskNewCatalog.sourceUrl -cne 'https://www.python.org/api/v2/downloads/release/?is_published=true' -or
            @($taskNewCatalog.versions | Where-Object { $_.downloadUrl -match '-embed-' }).Count -ne 0) {
            throw 'CLI repair retained the embedded Python catalog.'
        }
        $taskSummary.repair.oldCatalogRejected = $true
        Save-JsonReport $taskNewCatalog (Join-Path $taskReports 'legacy-catalog-after.json')
        $taskSummary.repair.stage = 'manifest-migration'
        $taskNewManifest = Assert-Manifest $taskLegacyInstallation $taskLegacyRoot $taskLegacyData $taskRepairRemote 'legacy-after'
        if ($taskNewManifest.operationId -ceq $taskLegacyId -or (Test-Path -LiteralPath $taskOldCentral)) {
            throw 'Repair did not replace the old embedded installation manifest.'
        }
        $taskSummary.repair.sourceUrlAfter = $taskNewManifest.sourceUrl
        $taskSummary.repair.sameVersion = $taskNewManifest.version -ceq $taskOldManifest.version
        if (-not $taskSummary.repair.sameVersion) { throw 'Repair unexpectedly changed the selected Python version.' }
        $taskSummary.repair.stage = 'development-runtime'
        $taskSummary.repair.runtime = Test-DevelopmentRuntime $taskLegacyInstallation $taskLegacy 'legacy-after'
        $taskSummary.repair.stage = 'complete'
        $taskSummary.repair.status = 'passed'
    } catch {
        $taskSummary.repair.status = 'failed'
        $taskSummary.repair.error = $_.Exception.Message
        $taskSummary.errors += "Repair: $($_.Exception.Message)"
    } finally {
        try {
            $taskSummary.repair.userEnvironment = if ((Get-UserEnvironmentFingerprint) -ceq $taskSummary.userEnvironmentBefore) { 'unchanged' } else { 'changed' }
            if ($taskSummary.repair.userEnvironment -cne 'unchanged') { throw 'Same-version repair unexpectedly changed the user environment.' }
        } catch {
            $taskSummary.repair.status = 'failed'
            $taskSummary.repair.error = @($taskSummary.repair.error,$_.Exception.Message) | Where-Object { $_ } | Join-String -Separator ' | '
            $taskSummary.errors += $_.Exception.Message
        }
    }
    $taskSummary.complete = $true
} catch {
    $taskSummary.errors += $_.Exception.Message
} finally {
    try {
        $taskSummary.userEnvironmentAfter = Get-UserEnvironmentFingerprint
        $taskSummary.userEnvironment = if ($taskSummary.userEnvironmentBefore -and $taskSummary.userEnvironmentBefore -ceq $taskSummary.userEnvironmentAfter) { 'unchanged' } else { 'changed-or-unverified' }
        if ($taskSummary.userEnvironment -cne 'unchanged') { $taskSummary.errors += 'The user registry environment changed or could not be verified; this script never writes or restores HKCU.' }
    } catch { $taskSummary.errors += "User environment verification failed: $($_.Exception.Message)" }
    $taskCleanupErrors = @()
    foreach ($taskScenario in @('fresh','legacy')) {
        foreach ($taskChild in @('tools','venv','data/downloads','temporary')) {
            try { Remove-RunDirectory (Join-Path $taskRun "$taskScenario/$taskChild") }
            catch { $taskCleanupErrors += $_.Exception.Message }
        }
    }
    $taskSummary.cleanup = if ($taskCleanupErrors.Count -eq 0) { 'passed' } else { 'failed' }
    $taskSummary.errors += $taskCleanupErrors
    foreach ($taskName in $taskEnvironmentNames) {
        [Environment]::SetEnvironmentVariable($taskName, $taskPreviousEnvironment[$taskName], 'Process')
    }
    $taskSummary.status = if ($taskSummary.complete -and $taskSummary.fresh.status -ceq 'passed' -and $taskSummary.repair.status -ceq 'passed' -and $taskSummary.errors.Count -eq 0) { 'passed' } else { 'failed' }
    $taskSummary.finishedAt = [DateTime]::UtcNow.ToString('o')
    Save-JsonReport $taskSummary (Join-Path $taskRun 'summary.json')
    Get-Content -LiteralPath (Join-Path $taskRun 'summary.json') -Raw | Write-Output
}
if ($taskSummary.status -cne 'passed') { throw "Python development runtime validation failed. See $taskRun\summary.json." }
