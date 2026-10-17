param([string]$Executable)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($env:CI -ne 'true' -or $env:GITHUB_ACTIONS -ne 'true' -or -not $IsWindows) {
    throw 'Run this isolated Python history check only on the Windows GitHub CI runner.'
}
$taskProject = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd('\','/')
if (-not $env:GITHUB_WORKSPACE -or -not $taskProject.Equals([IO.Path]::GetFullPath($env:GITHUB_WORKSPACE).TrimEnd('\','/'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The Python history check must run from the checked-out GitHub workspace.'
}
if ([IO.Path]::GetPathRoot($taskProject) -notin @('D:\','E:\')) {
    throw 'Python history installations, caches, and temporary files must stay on D: or E:.'
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
$taskRun = [IO.Path]::GetFullPath((Join-Path $taskRunParent ('python-history-' + [Guid]::NewGuid().ToString('N'))))
if (Test-Path -LiteralPath $taskRun) { throw "The isolated run already exists: $taskRun" }
New-Item -ItemType Directory -Path $taskRun | Out-Null
$taskRun = (Resolve-Path -LiteralPath $taskRun).ProviderPath.TrimEnd('\','/')
$taskRunPrefix = $taskRun + [IO.Path]::DirectorySeparatorChar
$taskData = Join-Path $taskRun 'data'
$taskReports = Join-Path $taskRun 'reports'
$taskCommandReport = Join-Path $taskReports 'catalog'
$taskCommandIndex = 0
$taskEnvironmentNames = @(
    'ENVNEXUS_AI_DATA_ROOT','TEMP','TMP','PIP_CACHE_DIR','PIP_CONFIG_FILE','PIP_NO_INDEX',
    'PIP_DISABLE_PIP_VERSION_CHECK','PYTHONUSERBASE','PYTHONPYCACHEPREFIX',
    'PYTHONHOME','PYTHONPATH','PYTHONSTARTUP','XDG_CACHE_HOME','UV_CACHE_DIR',
    'PIP_TARGET','PIP_PREFIX','PIP_ROOT','PIP_USER','PIP_LOG','HOME','USERPROFILE','PYTHONIOENCODING'
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
            if (($taskItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Refusing a reparse point: $taskAncestor" }
            $taskResolved = Get-NormalizedAbsolutePath (Resolve-Path -LiteralPath $taskAncestor).ProviderPath
            if (-not $taskResolved.StartsWith($taskRunPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw "Resolved path escapes the isolated run: $taskResolved" }
        }
        $taskAncestor = [IO.Path]::GetDirectoryName($taskAncestor)
    }
    if (((Get-Item -LiteralPath $taskRun -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'The isolated run root became a reparse point.' }
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
    $taskLink = Get-ChildItem -LiteralPath $taskAbsolute -Force -Recurse -ErrorAction Stop |
        Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 } | Select-Object -First 1
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
                [ordered]@{ name=$_; kind=$taskKey.GetValueKind($_).ToString(); value=$taskKey.GetValue($_, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) }
            })
        } finally { $taskKey.Dispose() }
    }
    $taskJson = ConvertTo-Json -InputObject ([ordered]@{ keyExists=($null -ne $taskKey); values=$taskValues }) -Depth 8 -Compress
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($taskJson))).ToLowerInvariant()
}

function Set-TemporaryEnvironment([string]$Directory) {
    foreach ($taskChild in @('tmp','pip-cache','user-base','pycache','xdg','uv','sentinels','fake-home','clean-home','untrusted','trusted')) { New-RunDirectory (Join-Path $Directory $taskChild) }
    $script:taskUntrustedDirectory = Assert-RunChildPath (Join-Path $Directory 'untrusted')
    $script:taskTrustedDirectory = Assert-RunChildPath (Join-Path $Directory 'trusted')
    $script:taskCleanHome = Assert-RunChildPath (Join-Path $Directory 'clean-home')
    $taskSentinels = Assert-RunChildPath (Join-Path $Directory 'sentinels')
    $taskFakeHome = Assert-RunChildPath (Join-Path $Directory 'fake-home')
    $taskDistutils = "[install]`ninstall_lib=$taskSentinels/distutils-lib`ninstall_scripts=$taskSentinels/distutils-scripts`nroot=$taskSentinels/distutils-root`n"
    foreach ($taskConfig in @((Join-Path $taskFakeHome 'pydistutils.cfg'),(Join-Path $script:taskUntrustedDirectory 'setup.cfg'))) {
        [IO.File]::WriteAllText((Assert-RunChildPath $taskConfig), $taskDistutils, [Text.UTF8Encoding]::new($false))
    }
    foreach ($taskModule in @('platform','runpy','pip')) {
        # Plain Python 2.4 syntax; relative sentinel paths remain ASCII even
        # when the interpreter itself is installed under a Unicode root.
        $taskSource = "import os`nopen(os.path.join(os.path.dirname(os.path.dirname(__file__)), 'sentinels', 'module-$taskModule.txt'), 'w').write('untrusted module imported')`nraise RuntimeError('untrusted smoke module imported')`n"
        [IO.File]::WriteAllText((Assert-RunChildPath (Join-Path $script:taskUntrustedDirectory "$taskModule.py")), $taskSource, [Text.UTF8Encoding]::new($false))
    }
    $taskPipConfig = Assert-RunChildPath (Join-Path $Directory 'pip.ini')
    [IO.File]::WriteAllText($taskPipConfig, '', [Text.UTF8Encoding]::new($false))
    $taskSettings = [ordered]@{
        ENVNEXUS_AI_DATA_ROOT=$taskData; TEMP=(Join-Path $Directory 'tmp'); TMP=(Join-Path $Directory 'tmp')
        PIP_CACHE_DIR=(Join-Path $Directory 'pip-cache'); PIP_CONFIG_FILE=$taskPipConfig
        PIP_NO_INDEX='1'; PIP_DISABLE_PIP_VERSION_CHECK='1'; PYTHONUSERBASE=(Join-Path $Directory 'user-base')
        PYTHONPYCACHEPREFIX=(Join-Path $Directory 'pycache'); PYTHONHOME=$null; PYTHONPATH=$null; PYTHONSTARTUP=$null
        XDG_CACHE_HOME=(Join-Path $Directory 'xdg'); UV_CACHE_DIR=(Join-Path $Directory 'uv')
        HOME=$taskFakeHome; USERPROFILE=$taskFakeHome; PYTHONIOENCODING='utf-8'
        PIP_TARGET=(Join-Path $taskSentinels 'pip-target'); PIP_PREFIX=(Join-Path $taskSentinels 'pip-prefix')
        PIP_ROOT=(Join-Path $taskSentinels 'pip-root'); PIP_USER=(Join-Path $taskSentinels 'pip-user'); PIP_LOG=(Join-Path $taskSentinels 'pip.log')
    }
    foreach ($taskName in $taskSettings.Keys) { [Environment]::SetEnvironmentVariable($taskName, $taskSettings[$taskName], 'Process') }
}

function Assert-NoSetupPollution([string]$Directory) {
    $taskSentinels = Assert-RunChildPath (Join-Path $Directory 'sentinels')
    $taskUnexpected = @(Get-ChildItem -LiteralPath $taskSentinels -Force -Recurse -ErrorAction Stop)
    if ($taskUnexpected.Count) { throw "Python setup used an untrusted module or an inherited installation target: $($taskUnexpected.FullName -join ', ')" }
    $taskUntrusted = Assert-RunChildPath (Join-Path $Directory 'untrusted')
    $taskFakeHome = Assert-RunChildPath (Join-Path $Directory 'fake-home')
    foreach ($taskItem in @(Get-ChildItem -LiteralPath $taskUntrusted -Force -Recurse) + @(Get-ChildItem -LiteralPath $taskFakeHome -Force -Recurse)) {
        $taskAllowed = if ($taskItem.DirectoryName -ieq $taskUntrusted) { @('platform.py','runpy.py','pip.py','setup.cfg') } elseif ($taskItem.DirectoryName -ieq $taskFakeHome) { @('pydistutils.cfg') } else { @() }
        if ($taskItem.PSIsContainer -or $taskItem.Name -notin $taskAllowed) { throw "Python setup wrote to an untrusted working directory or fake user home: $($taskItem.FullName)" }
    }
    Save-JsonReport ([ordered]@{
        status='passed'; sentinelCount=0; untrustedCwd=$taskUntrusted; fakeHome=$taskFakeHome
        inheritedVariables=@('PIP_TARGET','PIP_PREFIX','PIP_ROOT','PIP_USER','PIP_LOG')
        untrustedModules=@('platform','runpy','pip'); distutilsConfigurations=@('pydistutils.cfg','setup.cfg')
    }) (Join-Path $taskCommandReport 'setup-isolation.json')
}

function Invoke-CheckedCommand([string]$Program, [string[]]$CommandArguments, [string]$Label) {
    # Python 3.0 does not normalize CRLF in a command-line -c source string.
    $CommandArguments = @($CommandArguments | ForEach-Object { $_.Replace("`r`n","`n").Replace("`r","`n") })
    $taskProgram = Get-NormalizedAbsolutePath $Program
    $taskIsApp = $taskProgram -ieq (Get-NormalizedAbsolutePath $Executable)
    if (-not $taskIsApp) {
        $null = Assert-RunChildPath $taskProgram
        if ([IO.Path]::GetFileName($taskProgram) -notin @('python.exe','pip.exe')) { throw 'Only isolated Python interpreters and pip launchers may be executed.' }
    }
    $script:taskCommandIndex += 1
    $taskLog = Join-Path $taskCommandReport ('{0:D2}-{1}.json' -f $script:taskCommandIndex,$Label)
    $taskWatch = [Diagnostics.Stopwatch]::StartNew()
    $taskExit = -1
    $taskChildEnvironment = @{}
    $taskEncoding = [Console]::OutputEncoding
    $taskLocationPushed = $false
    try {
        # Only the application is exposed to the deliberate setup pollution.
        # Independent runtime checks use a clean home and trusted working
        # directory so their own venv creation cannot consume these traps.
        if (-not $taskIsApp) {
            foreach ($taskName in @('PIP_TARGET','PIP_PREFIX','PIP_ROOT','PIP_USER','PIP_LOG','HOME','USERPROFILE')) {
                $taskChildEnvironment[$taskName] = [Environment]::GetEnvironmentVariable($taskName, 'Process')
                $taskValue = if ($taskName -in @('HOME','USERPROFILE')) { $script:taskCleanHome } else { $null }
                [Environment]::SetEnvironmentVariable($taskName, $taskValue, 'Process')
            }
        }
        $taskWorkingDirectory = if ($taskIsApp) { $script:taskUntrustedDirectory } else { $script:taskTrustedDirectory }
        Push-Location -LiteralPath (Assert-RunChildPath $taskWorkingDirectory)
        $taskLocationPushed = $true
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        $taskOutput = & $taskProgram @CommandArguments 2>&1
        $taskExit = $LASTEXITCODE
    }
    catch { $taskOutput = @($_.Exception.Message) }
    finally {
        if ($taskLocationPushed) { Pop-Location }
        [Console]::OutputEncoding = $taskEncoding
        foreach ($taskName in $taskChildEnvironment.Keys) { [Environment]::SetEnvironmentVariable($taskName, $taskChildEnvironment[$taskName], 'Process') }
    }
    $taskWatch.Stop()
    $taskText = $taskOutput -join [Environment]::NewLine
    Save-JsonReport ([ordered]@{ program=$taskProgram; arguments=$CommandArguments; workingDirectory=$taskWorkingDirectory; deliberatelyPollutedSetup=$taskIsApp; exitCode=$taskExit; seconds=[Math]::Round($taskWatch.Elapsed.TotalSeconds,2); output=$taskText }) $taskLog
    if ($taskExit -ne 0) { throw "Python history command failed ($Label, exit $taskExit): $taskText" }
    $taskText
}

function Invoke-AppJson([string[]]$CommandArguments, [string]$Label) {
    # Installation and root selection never activate PATH or write HKCU. Do not
    # add use/repair/uninstall or invoke the downloaded Windows installers here.
    if ($CommandArguments[0] -notin @('versions','root','install')) { throw 'This smoke check does not authorize environment-changing CLI operations.' }
    $taskText = Invoke-CheckedCommand -Program $Executable -CommandArguments $CommandArguments -Label $Label
    try { $taskText | ConvertFrom-Json -NoEnumerate }
    catch { throw "CLI JSON could not be parsed for $Label. The full native output is retained in $taskCommandReport." }
}

function Assert-DownloadMetadata([object]$Remote) {
    if (-not $Remote -or -not $Remote.downloadUrl -or $null -ne $Remote.installUnavailableReason) { throw 'The historical release has no supported Windows package.' }
    $taskUrl = [Uri]$Remote.downloadUrl
    if ($taskUrl.Scheme -cne 'https' -or $taskUrl.Host -cne 'www.python.org' -or $taskUrl.UserInfo -or
        -not $taskUrl.AbsolutePath.StartsWith('/ftp/python/') -or $taskUrl.Query -or $taskUrl.Fragment -or
        $taskUrl.AbsolutePath -notmatch '(?i)\.(msi|exe|zip)$') { throw 'Expected an official Windows MSI, EXE, or full ZIP package.' }
    $taskLength = switch ([string]$Remote.checksumAlgorithm) { 'md5' {32}; 'sha1' {40}; 'sha256' {64}; 'sha512' {128}; default {0} }
    if ($taskLength -eq 0 -or [string]$Remote.checksum -notmatch "^[a-fA-F0-9]{$taskLength}$") { throw 'The historical package has no valid official digest.' }
}

function Assert-CachedCatalog([object]$Catalog, [string]$CacheHash, [string]$FetchedAt) {
    if (-not $Catalog.cached -or [string]$Catalog.fetchedAt -cne $FetchedAt -or
        (Get-FileHash -LiteralPath (Join-Path $taskData 'cache/version-sources/python.json') -Algorithm SHA256).Hash -cne $CacheHash) {
        throw 'A filtered versions command refreshed or modified the complete catalog cache.'
    }
}

function Assert-DateOrder([object[]]$Rows, [bool]$Ascending) {
    $taskPrevious = $null
    $taskMissingSeen = $false
    foreach ($taskRow in $Rows) {
        if (-not $taskRow.publishedAt) { $taskMissingSeen = $true; continue }
        if ($taskMissingSeen) { throw 'A dated release followed an undated release.' }
        $taskDate = [DateTimeOffset]::Parse([string]$taskRow.publishedAt).UtcDateTime
        if ($null -ne $taskPrevious -and (($Ascending -and $taskDate -lt $taskPrevious) -or (-not $Ascending -and $taskDate -gt $taskPrevious))) { throw 'CLI publication-date sorting is incorrect.' }
        $taskPrevious = $taskDate
    }
}

function Test-CatalogHistory {
    $taskCatalog = Invoke-AppJson -CommandArguments @('versions','python','--refresh','--json') -Label 'full-history'
    Save-JsonReport $taskCatalog (Join-Path $taskCommandReport 'full-catalog.json')
    if ($taskCatalog.toolId -cne 'python') { throw 'The catalog belongs to another tool.' }
    $taskRows = @($taskCatalog.versions)
    foreach ($taskRequired in @('2.7.18','3.10.11')) {
        $taskRemote = @($taskRows | Where-Object version -ceq $taskRequired)
        if ($taskRemote.Count -ne 1) { throw "The full history is missing or duplicates $taskRequired." }
        Assert-DownloadMetadata $taskRemote[0]
        $taskExpectedExtension = if ($taskRequired -ceq '2.7.18') { '.msi' } else { '.exe' }
        if (-not ([Uri]$taskRemote[0].downloadUrl).AbsolutePath.EndsWith($taskExpectedExtension, [StringComparison]::OrdinalIgnoreCase)) { throw "$taskRequired did not select its original MSI/EXE distribution." }
    }
    if (-not @($taskRows | Where-Object { $_.version -in @('1.6.1','0.9.1') })) { throw 'The catalog omitted the early Python archives.' }
    $taskArchives = @($taskRows | Where-Object channel -ceq 'archive')
    $taskPreviews = @($taskRows | Where-Object channel -ceq 'preview')
    $taskSourceOnly = @($taskRows | Where-Object { -not $_.downloadUrl })
    if (-not $taskArchives.Count -or -not $taskPreviews.Count -or -not $taskSourceOnly.Count) { throw 'The full history must retain archive, preview, and source-only records.' }
    foreach ($taskRow in $taskSourceOnly) {
        if ([string]::IsNullOrWhiteSpace([string]$taskRow.installUnavailableReason)) { throw "An unavailable release has no blocked reason: $($taskRow.version)" }
    }
    $taskCachePath = Assert-RunChildPath (Join-Path $taskData 'cache/version-sources/python.json')
    $taskCacheHash = (Get-FileHash -LiteralPath $taskCachePath -Algorithm SHA256).Hash
    $taskFetchedAt = [string]$taskCatalog.fetchedAt
    $taskQuery = Invoke-AppJson -CommandArguments @('versions','python','--query','3.10','--channel','stable','--sort','version-asc','--json') -Label 'query-stable-ascending'
    Assert-CachedCatalog $taskQuery $taskCacheHash $taskFetchedAt
    $taskExpected = @($taskRows | Where-Object { $_.version.Contains('3.10') -and $_.channel -ceq 'stable' })
    if (@($taskQuery.versions).Count -ne $taskExpected.Count -or -not @($taskQuery.versions | Where-Object version -ceq '3.10.11')) { throw 'CLI query/channel filtering lost the requested historical release.' }
    $taskPrevious = $null
    foreach ($taskRow in $taskQuery.versions) {
        if (-not $taskRow.version.Contains('3.10') -or $taskRow.channel -cne 'stable') { throw 'CLI query/channel filter returned an unrelated release.' }
        $taskNumeric = [version]$taskRow.version
        if ($null -ne $taskPrevious -and $taskNumeric -lt $taskPrevious) { throw 'CLI ascending numeric version sort is incorrect.' }
        $taskPrevious = $taskNumeric
    }
    $taskInstallable = Invoke-AppJson -CommandArguments @('versions','python','--query','2.7','--channel','stable','--installable','--sort','version-desc','--json') -Label 'installable-descending'
    Assert-CachedCatalog $taskInstallable $taskCacheHash $taskFetchedAt
    if (-not @($taskInstallable.versions | Where-Object version -ceq '2.7.18')) { throw 'The installable filter omitted Python 2.7.18.' }
    $taskPrevious = $null
    foreach ($taskRow in $taskInstallable.versions) {
        if (-not $taskRow.version.Contains('2.7') -or $taskRow.channel -cne 'stable') { throw 'The combined installable query returned another release family.' }
        Assert-DownloadMetadata $taskRow
        $taskNumeric = [version]$taskRow.version
        if ($null -ne $taskPrevious -and $taskNumeric -gt $taskPrevious) { throw 'CLI descending numeric version sort is incorrect.' }
        $taskPrevious = $taskNumeric
    }
    foreach ($taskChannel in @('archive','preview')) {
        $taskFiltered = Invoke-AppJson -CommandArguments @('versions','python','--channel',$taskChannel,'--json') -Label "channel-$taskChannel"
        Assert-CachedCatalog $taskFiltered $taskCacheHash $taskFetchedAt
        if (-not @($taskFiltered.versions).Count -or @($taskFiltered.versions | Where-Object channel -cne $taskChannel).Count) { throw "CLI channel filter is incorrect: $taskChannel" }
    }
    foreach ($taskDirection in @('asc','desc')) {
        $taskDated = Invoke-AppJson -CommandArguments @('versions','python','--channel','stable','--sort',"date-$taskDirection",'--json') -Label "publication-$taskDirection"
        Assert-CachedCatalog $taskDated $taskCacheHash $taskFetchedAt
        Assert-DateOrder @($taskDated.versions) ($taskDirection -ceq 'asc')
    }
    Save-JsonReport ([ordered]@{
        status='passed'; count=$taskRows.Count; archiveCount=$taskArchives.Count; previewCount=$taskPreviews.Count
        sourceOnlyCount=$taskSourceOnly.Count; unavailablePreviews=@($taskPreviews | Where-Object { -not $_.downloadUrl }).Count
        catalogSha256=$taskCacheHash.ToLowerInvariant(); fetchedAt=$taskFetchedAt; filteredCommandsUsedUnchangedCache=$true
        queryAndChannel='passed'; versionSort='passed'; dateSort='passed'; installableFilter='passed'
    }) (Join-Path $taskCommandReport 'validation.json')
    $taskCatalog
}

function Read-ProbeFields([string]$Text) {
    $taskFields = [ordered]@{}
    foreach ($taskLine in $Text -split '\r?\n') {
        $taskEquals = $taskLine.IndexOf('=')
        if ($taskEquals -gt 0) { $taskFields[$taskLine.Substring(0,$taskEquals)] = $taskLine.Substring($taskEquals+1) }
    }
    $taskFields
}

# Native output is decoded as UTF-8 by Invoke-CheckedCommand. Explicit byte
# output also works with Python 2.4 and -E, which ignores PYTHONIOENCODING.
$taskUtf8ProbeOutput = @'
import sys
def emit(name, value):
    if sys.version_info[0] >= 3:
        stream = getattr(sys.stdout, 'buffer', sys.stdout)
        stream.write(('%s=%s\n' % (name, value)).encode('utf-8'))
    else:
        if not isinstance(value, unicode):
            value = value.decode(getattr(sys, 'getfilesystemencoding', lambda: 'mbcs')() or 'mbcs')
        sys.stdout.write((unicode('%s=%s\n') % (name, value)).encode('utf-8'))
'@

function Test-HistoricalRuntime([string]$Installation, [string]$Scenario, [string]$Version, [bool]$HasPip, [bool]$HasVenv) {
    $taskPython = Assert-RunChildPath (Join-Path $Installation 'python.exe')
    [string[]]$taskFlags = if ($Version -match '^2\.[01]\.') { @('-S') } elseif ($Version -match '^2\.[23]\.') { @('-E','-S') } elseif ($Version.StartsWith('2.4.')) { @('-E') } else { @('-E','-s') }
    $taskStandardModule = if ($Version -match '^2\.[0-4]\.') { 'os' } else { 'json' }
    # This source deliberately uses Python 2.4 syntax: no with, json, pathlib,
    # f-strings, print function dependency, or newer interpreter switches.
    $taskProbe = $taskUtf8ProbeOutput + "`n" + @'
import os, sys
root = os.path.normcase(os.path.realpath(sys.argv[1]))
assert os.path.normcase(os.path.realpath(sys.prefix)) == root
assert os.path.normcase(os.path.realpath(sys.executable)).startswith(root + os.sep)
actual = '%d.%d.%d' % tuple(sys.version_info[:3])
assert actual == sys.argv[2]
__import__(sys.argv[3])
emit('version', actual)
emit('prefix', sys.prefix)
emit('executable', sys.executable)
emit('stdlib', sys.argv[3])
'@
    $taskRuntime = Read-ProbeFields (Invoke-CheckedCommand -Program $taskPython -CommandArguments @($taskFlags + @('-c',$taskProbe,$Installation,$Version,$taskStandardModule)) -Label 'runtime')
    if ($taskRuntime.version -cne $Version -or -not $taskRuntime.prefix) { throw 'The historical interpreter probe did not return the selected version and prefix.' }
    $taskChecks = [ordered]@{ version=$Version; runtime=$taskRuntime; pip='not-bundled-by-release'; venv='not-bundled-by-release' }
    if ($HasPip) {
        $taskPipProbe = $taskUtf8ProbeOutput + "`n" + @'
import os, sys, pip
root = os.path.normcase(os.path.realpath(sys.argv[1]))
assert os.path.normcase(os.path.realpath(pip.__file__)).startswith(root + os.sep)
emit('version', pip.__version__)
emit('module', pip.__file__)
'@
        $taskPipModule = Read-ProbeFields (Invoke-CheckedCommand -Program $taskPython -CommandArguments @($taskFlags + @('-c',$taskPipProbe,$Installation)) -Label 'pip-module')
        $taskPipList = Invoke-CheckedCommand -Program $taskPython -CommandArguments @($taskFlags + @('-m','pip','--disable-pip-version-check','--no-cache-dir','list')) -Label 'pip-module-list'
        if ($taskPipList -notmatch '(?im)^pip(?:\s|\s*\()') { throw 'The bundled pip module could not list pip.' }
        $taskPip = Assert-RunChildPath (Join-Path $Installation 'Scripts/pip.exe')
        $taskPipVersion = Invoke-CheckedCommand -Program $taskPip -CommandArguments @('--disable-pip-version-check','--version') -Label 'pip-exe'
        if (-not $taskPipVersion.ToLowerInvariant().Contains($Installation.ToLowerInvariant())) { throw 'The historical pip.exe does not refer to the final installation.' }
        $taskChecks.pip = [ordered]@{ status='passed'; module=$taskPipModule; list=$taskPipList; launcher=$taskPipVersion }
    }
    if ($HasVenv) {
        $taskVenv = Assert-RunChildPath (Join-Path $Scenario 'venv')
        # Python 3.3 venv naturally has no pip and has no --without-pip flag.
        $null = Invoke-CheckedCommand -Program $taskPython -CommandArguments @($taskFlags + @('-m','venv',$taskVenv)) -Label 'create-venv'
        $taskVenvPython = Assert-RunChildPath (Join-Path $taskVenv 'Scripts/python.exe')
        $taskVenvProbe = $taskUtf8ProbeOutput + "`n" + @'
import os, sys
env = os.path.normcase(os.path.realpath(sys.argv[1]))
root = os.path.normcase(os.path.realpath(sys.argv[2]))
assert os.path.normcase(os.path.realpath(sys.prefix)) == env
assert os.path.normcase(os.path.realpath(sys.base_prefix)) == root
assert sys.prefix != sys.base_prefix
assert os.path.normcase(os.path.realpath(sys.executable)).startswith(env + os.sep)
assert 2 + 2 == 4
emit('prefix', sys.prefix)
emit('basePrefix', sys.base_prefix)
emit('programResult', '4')
'@
        $taskVenvRuntime = Read-ProbeFields (Invoke-CheckedCommand -Program $taskVenvPython -CommandArguments @($taskFlags + @('-c',$taskVenvProbe,$taskVenv,$Installation)) -Label 'venv-runtime')
        if (-not $taskVenvRuntime.prefix -or $taskVenvRuntime.programResult -cne '4') { throw 'The historical venv did not execute its isolated interpreter.' }
        $taskVenvCheck = [ordered]@{ status='passed'; runtime=$taskVenvRuntime; pip='not-bundled-by-release' }
        if ($HasPip) {
            $taskVenvModule = Read-ProbeFields (Invoke-CheckedCommand -Program $taskVenvPython -CommandArguments @($taskFlags + @('-c',$taskPipProbe,$taskVenv)) -Label 'venv-pip-module')
            $taskVenvList = Invoke-CheckedCommand -Program $taskVenvPython -CommandArguments @($taskFlags + @('-m','pip','--disable-pip-version-check','--no-cache-dir','list')) -Label 'venv-pip-list'
            if ($taskVenvList -notmatch '(?im)^pip(?:\s|\s*\()') { throw 'The historical venv did not receive its bundled pip.' }
            $taskVenvPip = Assert-RunChildPath (Join-Path $taskVenv 'Scripts/pip.exe')
            $taskVenvLauncher = Invoke-CheckedCommand -Program $taskVenvPip -CommandArguments @('--disable-pip-version-check','--version') -Label 'venv-pip-exe'
            if (-not $taskVenvLauncher.ToLowerInvariant().Contains($taskVenv.ToLowerInvariant())) { throw 'The historical venv pip.exe points outside the venv.' }
            $taskVenvCheck.pip = [ordered]@{ status='passed'; module=$taskVenvModule; list=$taskVenvList; launcher=$taskVenvLauncher }
        } elseif ((Test-Path -LiteralPath (Join-Path $taskVenv 'Scripts/pip.exe')) -or (Test-Path -LiteralPath (Join-Path $taskVenv 'Lib/site-packages/pip'))) {
            throw 'Python 3.3 unexpectedly received pip from outside its official distribution.'
        }
        $taskChecks.venv = $taskVenvCheck
    }
    Save-JsonReport $taskChecks (Join-Path $taskCommandReport 'runtime-validation.json')
    $taskChecks
}

function Assert-InstalledManifest([object]$Remote, [string]$Installation, [string]$ManagedRoot, [string]$Version) {
    $taskManifest = Get-Content -LiteralPath (Assert-RunChildPath (Join-Path $Installation '.envpilot-install.json')) -Raw | ConvertFrom-Json
    if ($taskManifest.toolId -cne 'python' -or $taskManifest.version -cne $Version -or $taskManifest.sourceUrl -cne $Remote.downloadUrl -or
        $taskManifest.checksumAlgorithm -ine $Remote.checksumAlgorithm -or $taskManifest.checksum -ine $Remote.checksum -or
        (Get-NormalizedAbsolutePath $taskManifest.installationPath) -ine $Installation -or (Get-NormalizedAbsolutePath $taskManifest.managedRoot) -ine $ManagedRoot) {
        throw 'The managed Python manifest does not match the selected historical version, source, and root.'
    }
    $taskCentralPath = Assert-RunChildPath (Join-Path $taskData ('config/installations/' + $taskManifest.operationId + '.json'))
    $taskCentral = Get-Content -LiteralPath $taskCentralPath -Raw | ConvertFrom-Json
    if ($taskCentral.operationId -cne $taskManifest.operationId -or $taskCentral.sourceUrl -cne $Remote.downloadUrl -or
        $taskCentral.version -cne $Version -or (Get-NormalizedAbsolutePath $taskCentral.installationPath) -ine $Installation) { throw 'The central Python manifest does not match the installation.' }
    $taskFilename = [Uri]::UnescapeDataString([IO.Path]::GetFileName(([Uri]$Remote.downloadUrl).AbsolutePath))
    $taskArchive = Assert-RunChildPath (Join-Path $taskData "downloads/python/$taskFilename")
    $taskAlgorithm = ([string]$Remote.checksumAlgorithm).ToUpperInvariant()
    $taskActualHash = (Get-FileHash -LiteralPath $taskArchive -Algorithm $taskAlgorithm).Hash.ToLowerInvariant()
    if ($taskActualHash -ine $Remote.checksum) { throw 'The downloaded original package does not match its official digest.' }
    Save-JsonReport $taskManifest (Join-Path $taskCommandReport 'manifest.json')
    Save-JsonReport $taskCentral (Join-Path $taskCommandReport 'central-manifest.json')
    Save-JsonReport ([ordered]@{ sourceUrl=$Remote.downloadUrl; algorithm=$taskAlgorithm; digest=$taskActualHash; bytes=(Get-Item -LiteralPath $taskArchive).Length }) (Join-Path $taskCommandReport 'download.json')
    $taskManifest
}

$taskResults = @(
    [ordered]@{ version='2.0.1'; pip=$false; venv=$false }, [ordered]@{ version='2.1.3'; pip=$false; venv=$false },
    [ordered]@{ version='2.2.3'; pip=$false; venv=$false }, [ordered]@{ version='2.3.5'; pip=$false; venv=$false },
    [ordered]@{ version='2.4.4'; pip=$false; venv=$false }, [ordered]@{ version='2.7.18'; pip=$true; venv=$false },
    [ordered]@{ version='3.0.1'; pip=$false; venv=$false }, [ordered]@{ version='3.3.5'; pip=$false; venv=$true },
    [ordered]@{ version='3.4.4'; pip=$true; venv=$true }, [ordered]@{ version='3.10.11'; pip=$true; venv=$true }
) | ForEach-Object {
    [ordered]@{ version=$_.version; expectsPip=$_.pip; expectsVenv=$_.venv; status='not-started'; stage='prepare'; installationPath=$null; sourceUrl=$null; runtime=$null; setupIsolation='not-checked'; userEnvironment='not-checked'; cleanup='not-started'; error=$null }
}
$taskSummary = [ordered]@{
    artifactRoot=$taskRun; executable=$Executable; status='running'; complete=$false; startedAt=[DateTime]::UtcNow.ToString('o'); finishedAt=$null
    catalog='not-started'; catalogError=$null; userEnvironment='not-checked'; userEnvironmentBefore=$null; userEnvironmentAfter=$null
    results=@($taskResults); cleanup='not-started'; errors=@()
}
$taskCatalog = $null
try {
    foreach ($taskDirectory in @($taskData,$taskReports,$taskCommandReport,(Join-Path $taskData 'cache'))) { New-RunDirectory $taskDirectory }
    Save-JsonReport $taskSummary (Join-Path $taskRun 'summary.json')
    $taskSummary.userEnvironmentBefore = Get-UserEnvironmentFingerprint
    Set-TemporaryEnvironment (Join-Path $taskRun 'temporary/catalog')
    Save-JsonReport ([ordered]@{ schemaVersion=1; scannedAt=[DateTime]::UtcNow.ToString('o'); executables=@{}; manifests=@() }) (Join-Path $taskData 'cache/tool-executable-discovery.json')
    if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) { throw "Application executable not found: $Executable" }
    try { $taskCatalog = Test-CatalogHistory; $taskSummary.catalog = 'passed' }
    catch {
        $taskSummary.catalog = 'failed'; $taskSummary.catalogError = $_.Exception.Message
        $taskSummary.errors += "Catalog: $($_.Exception.Message)"
        # A filtering assertion must not prevent independent runtime evidence
        # when the one official query already saved a complete local catalog.
        $taskSavedCatalog = Assert-RunChildPath (Join-Path $taskData 'cache/version-sources/python.json')
        if (Test-Path -LiteralPath $taskSavedCatalog -PathType Leaf) {
            try {
                $taskRecoveredCatalog = Get-Content -LiteralPath $taskSavedCatalog -Raw | ConvertFrom-Json
                if ($taskRecoveredCatalog.toolId -cne 'python' -or -not @($taskRecoveredCatalog.versions).Count) { throw 'The saved catalog is not a Python release catalog.' }
                $taskCatalog = $taskRecoveredCatalog
            } catch { $taskSummary.errors += "Saved catalog could not be used for independent installation checks: $($_.Exception.Message)" }
        }
    }
    foreach ($taskResult in $taskResults) {
        $taskVersion = [string]$taskResult.version
        $taskScenario = Join-Path $taskRun "versions/$taskVersion"
        # Exercise old pip launchers in both non-ASCII scripts and Unicode paths.
        $taskRootName = if ($taskVersion -eq '3.10.11') { 'tools-开发-é' } else { 'tools' }
        $taskRoot = Join-Path $taskScenario $taskRootName
        $taskIsolationReady = $false
        $taskCommandReport = Join-Path $taskReports $taskVersion
        try {
            New-RunDirectory $taskCommandReport
            New-RunDirectory $taskRoot
            Set-TemporaryEnvironment (Join-Path $taskScenario 'temporary')
            $taskIsolationReady = $true
            $taskResult.stage = 'catalog-selection'
            if (-not $taskCatalog) { throw 'The complete official catalog was not available.' }
            $taskRemote = @($taskCatalog.versions | Where-Object version -ceq $taskVersion)
            if ($taskRemote.Count -ne 1) { throw "The production catalog is missing or duplicates $taskVersion." }
            $taskRemote = $taskRemote[0]
            Assert-DownloadMetadata $taskRemote
            $taskResult.sourceUrl = $taskRemote.downloadUrl
            Save-JsonReport $taskRemote (Join-Path $taskCommandReport 'selected-version.json')
            if ($taskVersion -match '^2\.' -or $taskVersion -match '^3\.[0-5]\.') {
                $taskUnsupportedRoot = Join-Path $taskScenario 'unsupported-开发-é'
                New-RunDirectory $taskUnsupportedRoot
                $null = Invoke-AppJson -CommandArguments @('root','set','python',$taskUnsupportedRoot,'--json') -Label 'select-incompatible-root'
                $taskBlocked = $false
                try { $null = Invoke-AppJson -CommandArguments @('install','python',$taskVersion,'--json') -Label 'incompatible-path-preview' }
                catch {
                    if ($_.Exception.Message -notmatch '非 ASCII') { throw }
                    $taskBlocked = $true
                    Save-JsonReport ([ordered]@{ status='passed'; reason=$_.Exception.Message; installationStarted=$false }) (Join-Path $taskCommandReport 'incompatible-path.json')
                }
                if (-not $taskBlocked -or (Test-Path -LiteralPath (Join-Path $taskUnsupportedRoot "python/$taskVersion"))) { throw 'An incompatible historical runtime path was not rejected before installation.' }
            }
            $taskResult.stage = 'install'
            $null = Invoke-AppJson -CommandArguments @('root','set','python',$taskRoot,'--json') -Label 'select-root'
            $taskInstalled = Invoke-AppJson -CommandArguments @('install','python',$taskVersion,'--yes','--json') -Label 'install'
            Save-JsonReport $taskInstalled (Join-Path $taskCommandReport 'installation.json')
            if ($taskInstalled.status -cne 'committed') { throw 'The historical Python installation did not commit.' }
            $taskInstallation = Assert-RunChildPath $taskInstalled.installationPath
            if ($taskInstallation -ine (Get-NormalizedAbsolutePath (Join-Path $taskRoot "python/$taskVersion"))) { throw 'The historical interpreter escaped its isolated version root.' }
            $taskResult.installationPath = $taskInstallation
            $taskResult.stage = 'manifest-and-digest'
            $null = Assert-InstalledManifest $taskRemote $taskInstallation $taskRoot $taskVersion
            $taskResult.stage = 'runtime'
            $taskResult.runtime = Test-HistoricalRuntime $taskInstallation $taskScenario $taskVersion $taskResult.expectsPip $taskResult.expectsVenv
            $taskResult.stage = 'complete'
            $taskResult.status = 'passed'
        } catch {
            $taskResult.status = 'failed'; $taskResult.error = $_.Exception.Message
            $taskSummary.errors += "$taskVersion : $($_.Exception.Message)"
        } finally {
            if ($taskIsolationReady) {
                try {
                    Assert-NoSetupPollution (Join-Path $taskScenario 'temporary')
                    $taskResult.setupIsolation = 'passed'
                } catch {
                    $taskResult.setupIsolation = 'failed'; $taskResult.status = 'failed'
                    $taskResult.error = @($taskResult.error,$_.Exception.Message) | Where-Object { $_ } | Join-String -Separator ' | '
                    $taskSummary.errors += $_.Exception.Message
                }
            }
            try {
                $taskResult.userEnvironment = if ($taskSummary.userEnvironmentBefore -and (Get-UserEnvironmentFingerprint) -ceq $taskSummary.userEnvironmentBefore) { 'unchanged' } else { 'changed-or-unverified' }
                if ($taskResult.userEnvironment -cne 'unchanged') { throw 'Installing a historical interpreter unexpectedly changed the user registry environment.' }
            } catch { $taskResult.status = 'failed'; $taskResult.error = @($taskResult.error,$_.Exception.Message) | Where-Object { $_ } | Join-String -Separator ' | '; $taskSummary.errors += $_.Exception.Message }
            $taskCleanupErrors = @()
            foreach ($taskCleanupPath in @($taskRoot,(Join-Path $taskScenario 'venv'),(Join-Path $taskScenario 'temporary'))) {
                try { Remove-RunDirectory $taskCleanupPath } catch { $taskCleanupErrors += $_.Exception.Message }
            }
            $taskResult.cleanup = if ($taskCleanupErrors.Count -eq 0) { 'passed' } else { 'failed' }
            if ($taskCleanupErrors.Count) { $taskResult.status = 'failed'; $taskSummary.errors += $taskCleanupErrors }
            Save-JsonReport $taskSummary (Join-Path $taskRun 'summary.json')
        }
    }
    $taskSummary.complete = $true
} catch { $taskSummary.errors += $_.Exception.Message }
finally {
    try {
        $taskSummary.userEnvironmentAfter = Get-UserEnvironmentFingerprint
        $taskSummary.userEnvironment = if ($taskSummary.userEnvironmentBefore -and $taskSummary.userEnvironmentBefore -ceq $taskSummary.userEnvironmentAfter) { 'unchanged' } else { 'changed-or-unverified' }
        if ($taskSummary.userEnvironment -cne 'unchanged') { $taskSummary.errors += 'HKCU verification failed; this script never writes or restores user registry values.' }
    } catch { $taskSummary.errors += "User environment verification failed: $($_.Exception.Message)" }
    $taskCleanupErrors = @()
    foreach ($taskPath in @((Join-Path $taskData 'downloads'),(Join-Path $taskRun 'temporary'))) {
        try { Remove-RunDirectory $taskPath } catch { $taskCleanupErrors += $_.Exception.Message }
    }
    $taskSummary.cleanup = if ($taskCleanupErrors.Count -eq 0) { 'passed' } else { 'failed' }
    $taskSummary.errors += $taskCleanupErrors
    foreach ($taskName in $taskEnvironmentNames) { [Environment]::SetEnvironmentVariable($taskName, $taskPreviousEnvironment[$taskName], 'Process') }
    $taskSummary.status = if ($taskSummary.complete -and $taskSummary.catalog -ceq 'passed' -and -not @($taskResults | Where-Object { $_.status -cne 'passed' }).Count -and $taskSummary.errors.Count -eq 0) { 'passed' } else { 'failed' }
    $taskSummary.finishedAt = [DateTime]::UtcNow.ToString('o')
    Save-JsonReport $taskSummary (Join-Path $taskRun 'summary.json')
    Get-Content -LiteralPath (Join-Path $taskRun 'summary.json') -Raw | Write-Output
}
if ($taskSummary.status -cne 'passed') { throw "Python history validation failed. See $taskRun\summary.json." }
