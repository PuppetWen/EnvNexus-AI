param([string]$Executable)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($env:CI -ne 'true' -or $env:GITHUB_ACTIONS -ne 'true' -or -not $IsWindows) {
    throw 'Run this isolated Maven installation check only on the Windows GitHub CI runner.'
}
$taskProject = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd('\','/')
if (-not $taskProject.Equals([IO.Path]::GetFullPath($env:GITHUB_WORKSPACE).TrimEnd('\','/'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The Maven check must run from the checked-out GitHub workspace.'
}
if ([IO.Path]::GetPathRoot($taskProject) -notin @('D:\','E:\')) {
    throw 'The isolated Maven installation, caches, and temporary files must be on D: or E:.'
}
if (-not $Executable) { $Executable = Join-Path $taskProject 'src-tauri/target/release/envnexus-ai.exe' }
$Executable = [IO.Path]::GetFullPath($Executable)
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) { throw "Application executable not found: $Executable" }
$taskRunParent = Join-Path $taskProject 'artifacts'
$taskAncestor = $taskRunParent
while ($taskAncestor) {
    if ((Test-Path -LiteralPath $taskAncestor) -and
        ((Get-Item -LiteralPath $taskAncestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing an artifact directory through a reparse point: $taskAncestor"
    }
    $taskAncestor = [IO.Path]::GetDirectoryName($taskAncestor)
}
$taskRun = [IO.Path]::GetFullPath((Join-Path $taskRunParent ('maven-' + [Guid]::NewGuid().ToString('N'))))
if (Test-Path -LiteralPath $taskRun) { throw "The isolated run already exists: $taskRun" }
New-Item -ItemType Directory -Path $taskRun | Out-Null
$taskRun = (Resolve-Path -LiteralPath $taskRun).ProviderPath.TrimEnd('\','/')
$taskRunPrefix = $taskRun + [IO.Path]::DirectorySeparatorChar
$taskReports = Join-Path $taskRun 'reports'
New-Item -ItemType Directory -Path $taskReports | Out-Null
$taskCommandIndex = 0
$taskVersion = '3.9.16'
$taskSourceUrl = "https://downloads.apache.org/maven/maven-3/$taskVersion/binaries/apache-maven-$taskVersion-bin.zip"
$taskEnvironmentNames = @(
    'ENVNEXUS_AI_DATA_ROOT','TEMP','TMP','JAVA_HOME','JAVACMD','PATH','HOME','USERPROFILE',
    'MAVEN_SKIP_RC','MAVEN_HOME','M2_HOME','MAVEN_OPTS','MAVEN_DEBUG_OPTS','MAVEN_ARGS','MAVEN_BASEDIR',
    'MAVEN_PROJECTBASEDIR','JVM_CONFIG_MAVEN_PROPS','MAVEN_BATCH_PAUSE','MAVEN_TERMINATE_CMD',
    'JAVA_TOOL_OPTIONS','_JAVA_OPTIONS','JDK_JAVA_OPTIONS'
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
                throw "Refusing an isolated path through a reparse point: $taskAncestor"
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

function Get-RegistryEnvironmentFingerprint([ValidateSet('User','Machine')][string]$Scope) {
    $taskHive = if ($Scope -ceq 'User') { [Microsoft.Win32.Registry]::CurrentUser } else { [Microsoft.Win32.Registry]::LocalMachine }
    $taskKeyPath = if ($Scope -ceq 'User') { 'Environment' } else { 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment' }
    $taskKey = $taskHive.OpenSubKey($taskKeyPath, $false)
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

function Invoke-RecordedCommand(
    [string]$Program, [string[]]$CommandArguments, [string]$Label,
    [int]$TimeoutSeconds = 120, [hashtable]$EnvironmentOverrides = @{}, [switch]$AllowFailure
) {
    $script:taskCommandIndex += 1
    $taskLog = Join-Path $taskReports ('{0:D2}-{1}.json' -f $script:taskCommandIndex,$Label)
    $taskStart = [Diagnostics.ProcessStartInfo]::new()
    $taskStart.UseShellExecute = $false
    $taskStart.CreateNoWindow = $true
    $taskStart.RedirectStandardInput = $true
    $taskStart.RedirectStandardOutput = $true
    $taskStart.RedirectStandardError = $true
    $taskStart.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $taskStart.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $taskStart.WorkingDirectory = Assert-RunChildPath (Join-Path $taskRun 'untrusted-project')
    if ([IO.Path]::GetExtension($Program) -ieq '.cmd') {
        # cmd.exe requires one outer quote pair around the quoted batch path.
        # This smoke invokes only the fixed -v argument; reject metacharacters
        # with expansion semantics before composing its command line.
        if ($Program -match '["%!\r\n]' -or ($CommandArguments -join ' ') -cne '-v') { throw 'Unsupported batch probe command.' }
        $taskStart.FileName = Join-Path ([Environment]::SystemDirectory) 'cmd.exe'
        $taskStart.Arguments = '/d /s /c ""' + $Program + '" -v"'
    } else {
        $taskStart.FileName = $Program
        foreach ($taskArgument in $CommandArguments) { $taskStart.ArgumentList.Add($taskArgument) }
    }
    foreach ($taskName in $EnvironmentOverrides.Keys) {
        if ($null -eq $EnvironmentOverrides[$taskName]) { $null = $taskStart.Environment.Remove($taskName) }
        else { $taskStart.Environment[$taskName] = [string]$EnvironmentOverrides[$taskName] }
    }
    $taskProcess = [Diagnostics.Process]::new()
    $taskProcess.StartInfo = $taskStart
    $taskWatch = [Diagnostics.Stopwatch]::StartNew()
    $taskExit = -1
    $taskTimedOut = $false
    $taskText = ''
    try {
        if (-not $taskProcess.Start()) { throw 'Could not start the smoke command.' }
        $taskProcess.StandardInput.Close()
        $taskOut = $taskProcess.StandardOutput.ReadToEndAsync()
        $taskErr = $taskProcess.StandardError.ReadToEndAsync()
        if (-not $taskProcess.WaitForExit($TimeoutSeconds * 1000)) {
            $taskTimedOut = $true
            $taskProcess.Kill($true)
            if (-not $taskProcess.WaitForExit(10000)) { throw 'The timed-out smoke process did not exit.' }
        }
        $taskExit = $taskProcess.ExitCode
        $taskText = @($taskOut.GetAwaiter().GetResult(),$taskErr.GetAwaiter().GetResult()) | Where-Object { $_ } | Join-String -Separator "`n"
    } catch { $taskText = @($taskText,$_.Exception.Message) | Where-Object { $_ } | Join-String -Separator "`n" }
    finally { $taskWatch.Stop(); $taskProcess.Dispose() }
    $taskResult = [ordered]@{
        program=$Program; arguments=$CommandArguments; exitCode=$taskExit; timedOut=$taskTimedOut
        workingDirectory=$taskStart.WorkingDirectory; seconds=[Math]::Round($taskWatch.Elapsed.TotalSeconds,2); output=$taskText
    }
    Save-JsonReport $taskResult $taskLog
    if ($taskTimedOut -or (-not $AllowFailure -and $taskExit -ne 0)) {
        throw "Maven smoke command failed ($Label, exit $taskExit, timeout=$taskTimedOut): $taskText"
    }
    [pscustomobject]$taskResult
}

function Invoke-AppJson([string[]]$CommandArguments, [string]$Label, [int]$TimeoutSeconds = 120) {
    $taskResult = Invoke-RecordedCommand -Program $Executable -CommandArguments $CommandArguments -Label $Label -TimeoutSeconds $TimeoutSeconds
    $taskResult.output | ConvertFrom-Json -NoEnumerate
}

function Get-JavaMajor([string]$Output) {
    if ($Output -notmatch '(?m)(?:version\s+"?|javac\s+|openjdk\s+)([0-9]+)(?:\.([0-9]+))?') {
        throw "Could not identify the JDK version: $Output"
    }
    if ($Matches[1] -ceq '1') { return [int]$Matches[2] }
    [int]$Matches[1]
}

$taskSummary = [ordered]@{
    status='running'; startedAt=[DateTime]::UtcNow.ToString('o'); executable=$Executable
    version=$taskVersion; runRoot=$taskRun; complete=$false; stage='setup'
    userEnvironmentBefore=$null; userEnvironmentAfter=$null; machineEnvironmentBefore=$null; machineEnvironmentAfter=$null
    userEnvironment='unverified'; machineEnvironment='unverified'; cleanup='pending'; errors=@()
    registryJavaHomeUser=[Environment]::GetEnvironmentVariable('JAVA_HOME','User')
    registryJavaHomeMachine=[Environment]::GetEnvironmentVariable('JAVA_HOME','Machine')
    runnerJdk=$null; selectedJdk=$null; sourceUrl=$null; sha512=$null
    invalidJavaHomeRejected=$false; processEnvironmentUnchanged=$false; startupHooksSkipped=$false
    installationPath=$null; runtime=$null; activationPreviewOnly=$false
    coverage='The process JAVA_HOME is deliberately invalid. Fresh valid registry JDKs retain priority; otherwise the runner PATH JDK must be usable. No-JDK cases are covered by backend unit tests without modifying registry values.'
}
try {
    $taskSummary.userEnvironmentBefore = Get-RegistryEnvironmentFingerprint 'User'
    $taskSummary.machineEnvironmentBefore = Get-RegistryEnvironmentFingerprint 'Machine'
    foreach ($taskChild in @('tools','data/cache','temporary','home','untrusted-project/.mvn')) {
        New-RunDirectory (Join-Path $taskRun $taskChild)
    }
    $taskData = Assert-RunChildPath (Join-Path $taskRun 'data')
    $taskManagedRoot = Assert-RunChildPath (Join-Path $taskRun 'tools')
    $taskPoisonHome = Assert-RunChildPath (Join-Path $taskRun 'missing-jdk')
    $taskSentinel = Assert-RunChildPath (Join-Path $taskRun 'home/startup-hook-ran.txt')
    if ($taskRun -match '["%!\r\n]') { throw 'This workspace path cannot safely be used by the batch startup sentinel.' }
    foreach ($taskName in @('mavenrc_pre.cmd','mavenrc_post.cmd','mavenrc_pre.bat','mavenrc_post.bat')) {
        [IO.File]::WriteAllText((Assert-RunChildPath (Join-Path $taskRun "home/$taskName")), "@echo startup hook ran>>`"$taskSentinel`"`r`nexit /b 91`r`n", [Text.ASCIIEncoding]::new())
    }
    [IO.File]::WriteAllText((Assert-RunChildPath (Join-Path $taskRun 'untrusted-project/.mvn/jvm.config')), '-XX:EnvNexusSmokeMustNotLoadProjectConfig', [Text.ASCIIEncoding]::new())
    Save-JsonReport ([ordered]@{ schemaVersion=1; scannedAt=[DateTime]::UtcNow.ToString('o'); executables=@{}; manifests=@() }) (Join-Path $taskData 'cache/tool-executable-discovery.json')

    # Use an already provisioned runner JDK; this script never installs Java.
    $taskJdkCandidates = @($taskPreviousEnvironment.JAVA_HOME)
    foreach ($taskName in @('JAVA_HOME_21_X64','JAVA_HOME_17_X64','JAVA_HOME_11_X64','JAVA_HOME_8_X64')) {
        $taskJdkCandidates += [Environment]::GetEnvironmentVariable($taskName,'Process')
    }
    foreach ($taskPart in ($taskPreviousEnvironment.PATH -split ';')) {
        $taskPart = $taskPart.Trim().Trim('"')
        if ($taskPart -and [IO.Path]::GetFileName($taskPart.TrimEnd('\','/')) -ieq 'bin') {
            $taskJdkCandidates += [IO.Path]::GetDirectoryName($taskPart.TrimEnd('\','/'))
        }
    }
    $taskRunnerJdk = $null
    foreach ($taskCandidate in ($taskJdkCandidates | Where-Object { $_ } | Select-Object -Unique)) {
        $taskCandidate = Get-NormalizedAbsolutePath $taskCandidate.Trim('"')
        if ((Test-Path -LiteralPath (Join-Path $taskCandidate 'bin/java.exe') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $taskCandidate 'bin/javac.exe') -PathType Leaf)) {
            $taskRunnerJdk = $taskCandidate
            break
        }
    }
    if (-not $taskRunnerJdk) { throw 'The runner must already provide a complete JDK with java.exe and javac.exe.' }
    $taskCleanJava = @{ JAVA_TOOL_OPTIONS=$null; _JAVA_OPTIONS=$null; JDK_JAVA_OPTIONS=$null; TEMP=(Join-Path $taskRun 'temporary'); TMP=(Join-Path $taskRun 'temporary') }
    $taskJava = Invoke-RecordedCommand -Program (Join-Path $taskRunnerJdk 'bin/java.exe') -CommandArguments @("-Duser.home=$(Join-Path $taskRun 'home')","-Djava.io.tmpdir=$(Join-Path $taskRun 'temporary')",'-version') -Label 'runner-java' -TimeoutSeconds 30 -EnvironmentOverrides $taskCleanJava
    $taskJavac = Invoke-RecordedCommand -Program (Join-Path $taskRunnerJdk 'bin/javac.exe') -CommandArguments @("-J-Duser.home=$(Join-Path $taskRun 'home')","-J-Djava.io.tmpdir=$(Join-Path $taskRun 'temporary')",'-version') -Label 'runner-javac' -TimeoutSeconds 30 -EnvironmentOverrides $taskCleanJava
    $taskJavaMajor = Get-JavaMajor $taskJava.output
    if ($taskJavaMajor -lt 8 -or $taskJavaMajor -ne (Get-JavaMajor $taskJavac.output)) { throw 'The runner java and javac must agree on a JDK version of at least 8.' }
    $taskSummary.runnerJdk = [ordered]@{ home=$taskRunnerJdk; major=$taskJavaMajor }

    $taskPoisonEnvironment = @{
        ENVNEXUS_AI_DATA_ROOT=$taskData; TEMP=(Join-Path $taskRun 'temporary'); TMP=(Join-Path $taskRun 'temporary')
        JAVA_HOME=$taskPoisonHome; JAVACMD=(Join-Path $taskPoisonHome 'bin/java.exe')
        PATH=((Join-Path $taskRunnerJdk 'bin') + ';' + $taskPreviousEnvironment.PATH)
        HOME=(Join-Path $taskRun 'home'); USERPROFILE=(Join-Path $taskRun 'home'); MAVEN_SKIP_RC=$null
        MAVEN_HOME=(Join-Path $taskRun 'missing-maven'); M2_HOME=(Join-Path $taskRun 'missing-m2')
        MAVEN_OPTS='-XX:EnvNexusSmokeMustNotInheritMavenOpts'; MAVEN_DEBUG_OPTS='-XX:EnvNexusSmokeMustNotInheritDebugOpts'
        MAVEN_ARGS='--envnexus-smoke-must-not-inherit-args'; MAVEN_BASEDIR=(Join-Path $taskRun 'untrusted-project')
        MAVEN_PROJECTBASEDIR=(Join-Path $taskRun 'untrusted-project'); JVM_CONFIG_MAVEN_PROPS='-XX:EnvNexusSmokeMustNotInheritJvmConfig'
        MAVEN_BATCH_PAUSE=$null; MAVEN_TERMINATE_CMD=$null
        JAVA_TOOL_OPTIONS='-XX:EnvNexusSmokeMustNotInheritJavaToolOptions'
        _JAVA_OPTIONS='-XX:EnvNexusSmokeMustNotInheritJavaOptions'; JDK_JAVA_OPTIONS='-XX:EnvNexusSmokeMustNotInheritJdkOptions'
    }
    foreach ($taskName in $taskPoisonEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($taskName, $taskPoisonEnvironment[$taskName], 'Process')
    }
    Save-JsonReport ([ordered]@{
        javaHome=$taskPoisonHome; pathJdk=$taskRunnerJdk; startupSentinel=$taskSentinel
        poisonedVariables=@($taskPoisonEnvironment.Keys | Sort-Object)
        registryJavaHomeUser=$taskSummary.registryJavaHomeUser; registryJavaHomeMachine=$taskSummary.registryJavaHomeMachine
    }) (Join-Path $taskReports 'poisoned-process-environment.json')
    $null = Invoke-AppJson -CommandArguments @('root','set','maven',$taskManagedRoot,'--json') -Label 'root'
    $taskSummary.stage = 'official-catalog'
    $taskCatalog = Invoke-AppJson -CommandArguments @('versions','maven','--refresh','--json') -Label 'catalog'
    $taskRemote = @($taskCatalog.versions | Where-Object version -ceq $taskVersion)
    if ($taskRemote.Count -ne 1 -or $taskRemote[0].downloadUrl -cne $taskSourceUrl -or
        $taskRemote[0].checksumAlgorithm -ine 'sha512' -or $taskRemote[0].checksum -notmatch '^[a-fA-F0-9]{128}$') {
        throw 'The catalog must provide Maven 3.9.16 from the official Apache ZIP with its SHA-512.'
    }
    $taskRemote = $taskRemote[0]
    Save-JsonReport $taskRemote (Join-Path $taskReports 'selected-release.json')
    $taskSummary.sourceUrl = $taskRemote.downloadUrl
    $taskSummary.sha512 = $taskRemote.checksum
    $taskSummary.stage = 'preview'
    $taskPlan = Invoke-AppJson -CommandArguments @('install','maven',$taskVersion,'--json') -Label 'install-preview'
    $taskJavaWarning = @($taskPlan.warnings | Where-Object { $_ -match '安装验证将临时使用 JDK .+JAVA_HOME=' })
    if ($taskPlan.actionKind -cne 'install' -or @($taskPlan.environmentDiffs).Count -ne 0 -or
        $taskJavaWarning.Count -ne 1 -or $taskJavaWarning[0] -notmatch '安装验证将临时使用 JDK .+?，JAVA_HOME=(.+?)。这只作用于 Maven 验证子进程') {
        throw 'Install preview must identify its temporary JDK without proposing global environment activation.'
    }
    $taskSelectedJdk = Get-NormalizedAbsolutePath $Matches[1]
    if ($taskSelectedJdk -ieq $taskPoisonHome -or
        -not (Test-Path -LiteralPath (Join-Path $taskSelectedJdk 'bin/java.exe') -PathType Leaf) -or
        -not (Test-Path -LiteralPath (Join-Path $taskSelectedJdk 'bin/javac.exe') -PathType Leaf)) {
        throw 'The selected Maven validation JDK is incomplete or is the poisoned JAVA_HOME.'
    }
    $taskSummary.selectedJdk = [ordered]@{ home=$taskSelectedJdk; warning=$taskJavaWarning[0] }
    if (Test-Path -LiteralPath (Join-Path $taskManagedRoot "maven/$taskVersion")) { throw 'Preview unexpectedly installed Maven.' }

    $taskSummary.stage = 'install'
    $taskInstalled = Invoke-AppJson -CommandArguments @('install','maven',$taskVersion,'--yes','--json') -Label 'install' -TimeoutSeconds 300
    if ($taskInstalled.status -cne 'committed') { throw 'Maven installation did not commit.' }
    $taskInstallation = Assert-RunChildPath $taskInstalled.installationPath
    if ($taskInstallation -ine (Get-NormalizedAbsolutePath (Join-Path $taskManagedRoot "maven/$taskVersion"))) {
        throw 'Maven was not installed at the requested isolated version directory.'
    }
    $taskSummary.installationPath = $taskInstallation
    $taskSummary.stage = 'manifest'
    $taskManifest = Get-Content -LiteralPath (Assert-RunChildPath (Join-Path $taskInstallation '.envpilot-install.json')) -Raw | ConvertFrom-Json
    if ($taskManifest.toolId -cne 'maven' -or $taskManifest.version -cne $taskVersion -or
        $taskManifest.sourceUrl -cne $taskSourceUrl -or $taskManifest.checksumAlgorithm -ine 'sha512' -or
        $taskManifest.checksum -ine $taskRemote.checksum -or
        (Get-NormalizedAbsolutePath $taskManifest.installationPath) -ine $taskInstallation -or
        (Get-NormalizedAbsolutePath $taskManifest.managedRoot) -ine $taskManagedRoot) {
        throw 'The installed Maven manifest does not match the official release and isolated destination.'
    }
    $taskCentral = Get-Content -LiteralPath (Assert-RunChildPath (Join-Path $taskData ('config/installations/' + $taskManifest.operationId + '.json'))) -Raw | ConvertFrom-Json
    if ($taskCentral.operationId -cne $taskManifest.operationId -or $taskCentral.checksum -ine $taskRemote.checksum -or
        $taskCentral.sourceUrl -cne $taskSourceUrl -or (Get-NormalizedAbsolutePath $taskCentral.installationPath) -ine $taskInstallation) {
        throw 'The central installation manifest does not match the installed Maven manifest.'
    }
    Save-JsonReport $taskManifest (Join-Path $taskReports 'installation-manifest.json')
    $taskArchive = Assert-RunChildPath (Join-Path $taskData "downloads/maven/apache-maven-$taskVersion-bin.zip")
    $taskHash = (Get-FileHash -LiteralPath $taskArchive -Algorithm SHA512).Hash.ToLowerInvariant()
    if ($taskHash -ine $taskRemote.checksum) { throw 'The downloaded Maven ZIP does not match the official SHA-512.' }
    Save-JsonReport ([ordered]@{ sourceUrl=$taskSourceUrl; sha512=$taskHash; bytes=(Get-Item -LiteralPath $taskArchive).Length }) (Join-Path $taskReports 'download.json')
    foreach ($taskRelative in @('bin/mvn.cmd',"lib/maven-core-$taskVersion.jar",'bin/m2.conf','conf/settings.xml')) {
        if (-not (Test-Path -LiteralPath (Assert-RunChildPath (Join-Path $taskInstallation $taskRelative)) -PathType Leaf)) {
            throw "The complete Maven package is missing $taskRelative."
        }
    }
    if (Test-Path -LiteralPath $taskSentinel) { throw 'Maven validation executed a poisoned user startup hook.' }
    $taskSummary.startupHooksSkipped = $true

    $taskSummary.stage = 'real-version'
    $taskCleanMaven = @{}
    foreach ($taskName in @('MAVEN_HOME','M2_HOME','MAVEN_OPTS','MAVEN_DEBUG_OPTS','MAVEN_ARGS','MAVEN_BASEDIR','MAVEN_PROJECTBASEDIR','JVM_CONFIG_MAVEN_PROPS','MAVEN_BATCH_PAUSE','MAVEN_TERMINATE_CMD','JAVA_TOOL_OPTIONS','_JAVA_OPTIONS','JDK_JAVA_OPTIONS')) {
        $taskCleanMaven[$taskName] = $null
    }
    $taskCleanMaven.MAVEN_SKIP_RC = '1'
    $taskCleanMaven.MAVEN_BASEDIR = $taskInstallation
    $taskCleanMaven.MAVEN_OPTS = '-Duser.home="' + (Join-Path $taskRun 'home') + '" -Djava.io.tmpdir="' + (Join-Path $taskRun 'temporary') + '"'
    $taskCleanMaven.JAVA_HOME = $taskSelectedJdk
    $taskCleanMaven.PATH = (Join-Path $taskSelectedJdk 'bin') + ';' + $taskPreviousEnvironment.PATH
    $taskMaven = Assert-RunChildPath (Join-Path $taskInstallation 'bin/mvn.cmd')
    $taskVersionResult = Invoke-RecordedCommand -Program $taskMaven -CommandArguments @('-v') -Label 'maven-version' -TimeoutSeconds 60 -EnvironmentOverrides $taskCleanMaven
    $taskVersionOutput = $taskVersionResult.output -replace '\x1b\[[0-?]*[ -/]*[@-~]',''
    if ($taskVersionOutput -notmatch '(?m)^Apache Maven 3\.9\.16(?:\s|$)' -or
        $taskVersionOutput -notmatch '(?m)^Maven home:\s*(.+?)\s*$' -or
        (Get-NormalizedAbsolutePath $Matches[1]) -ine $taskInstallation) { throw 'The installed command did not report the selected Maven release and home.' }
    if ($taskVersionOutput -notmatch 'runtime:\s*([^\r\n]+)') { throw 'Maven did not report its actual Java runtime.' }
    $taskActualJavaRuntime = Get-NormalizedAbsolutePath $Matches[1].Trim()
    if ($taskActualJavaRuntime -ine $taskSelectedJdk -and $taskActualJavaRuntime -ine (Join-Path $taskSelectedJdk 'jre')) {
        throw 'Maven ran with a Java runtime outside the JDK selected by the install plan.'
    }
    $taskSummary.runtime = [ordered]@{ javaRuntime=$taskActualJavaRuntime; output=$taskVersionOutput }

    # Prove that the original Maven script still rejects this exact invalid
    # JAVA_HOME, so a committed app install exercises its temporary override.
    $taskCleanMaven.JAVA_HOME = $taskPoisonHome
    $taskRejected = Invoke-RecordedCommand -Program $taskMaven -CommandArguments @('-v') -Label 'maven-invalid-java-home' -TimeoutSeconds 30 -EnvironmentOverrides $taskCleanMaven -AllowFailure
    if ($taskRejected.exitCode -eq 0 -or $taskRejected.output -notmatch 'JAVA_HOME') {
        throw 'The poisoned JAVA_HOME baseline did not reproduce the original Maven startup failure.'
    }
    $taskSummary.invalidJavaHomeRejected = $true
    $taskSummary.stage = 'activation-preview'
    $taskActivation = Invoke-AppJson -CommandArguments @('use','maven',$taskInstallation,'--json') -Label 'activation-preview'
    if ($taskActivation.actionKind -cne 'switch' -or
        @($taskActivation.environmentDiffs | Where-Object variable -ieq 'JAVA_HOME').Count -ne 0) {
        throw 'Maven activation preview must not change the default Java environment.'
    }
    $taskSummary.activationPreviewOnly = $true
    foreach ($taskName in $taskPoisonEnvironment.Keys) {
        $taskActual = [Environment]::GetEnvironmentVariable($taskName, 'Process')
        $taskExpected = $taskPoisonEnvironment[$taskName]
        if (-not [string]::Equals($taskActual, $taskExpected, [StringComparison]::Ordinal)) {
            throw "The Maven child-process check changed its caller environment: $taskName"
        }
    }
    if (Test-Path -LiteralPath $taskSentinel) { throw 'A user Maven startup hook ran during validation.' }
    $taskSummary.processEnvironmentUnchanged = $true
    $taskSummary.stage = 'complete'
    $taskSummary.complete = $true
} catch { $taskSummary.errors += $_.Exception.Message }
finally {
    try {
        $taskSummary.userEnvironmentAfter = Get-RegistryEnvironmentFingerprint 'User'
        $taskSummary.machineEnvironmentAfter = Get-RegistryEnvironmentFingerprint 'Machine'
        $taskSummary.userEnvironment = if ($taskSummary.userEnvironmentBefore -and $taskSummary.userEnvironmentBefore -ceq $taskSummary.userEnvironmentAfter) { 'unchanged' } else { 'changed-or-unverified' }
        $taskSummary.machineEnvironment = if ($taskSummary.machineEnvironmentBefore -and $taskSummary.machineEnvironmentBefore -ceq $taskSummary.machineEnvironmentAfter) { 'unchanged' } else { 'changed-or-unverified' }
        if ($taskSummary.userEnvironment -cne 'unchanged' -or $taskSummary.machineEnvironment -cne 'unchanged') {
            $taskSummary.errors += 'The user or machine registry environment changed or could not be verified; this script never writes or restores registry values.'
        }
    } catch { $taskSummary.errors += "Registry environment verification failed: $($_.Exception.Message)" }
    $taskCleanupErrors = @()
    foreach ($taskChild in @('tools','data/downloads','temporary','home','untrusted-project')) {
        try { Remove-RunDirectory (Join-Path $taskRun $taskChild) }
        catch { $taskCleanupErrors += $_.Exception.Message }
    }
    $taskSummary.cleanup = if ($taskCleanupErrors.Count -eq 0) { 'passed' } else { 'failed' }
    $taskSummary.errors += $taskCleanupErrors
    foreach ($taskName in $taskEnvironmentNames) {
        [Environment]::SetEnvironmentVariable($taskName, $taskPreviousEnvironment[$taskName], 'Process')
    }
    $taskSummary.status = if ($taskSummary.complete -and $taskSummary.errors.Count -eq 0) { 'passed' } else { 'failed' }
    $taskSummary.finishedAt = [DateTime]::UtcNow.ToString('o')
    Save-JsonReport $taskSummary (Join-Path $taskRun 'summary.json')
    Get-Content -LiteralPath (Join-Path $taskRun 'summary.json') -Raw | Write-Output
}
if ($taskSummary.status -cne 'passed') { throw "Maven JDK and isolated installation validation failed. See $taskRun\summary.json." }
