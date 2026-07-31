param(
    [string]$Executable
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
if (-not $Executable) {
    $Executable = Join-Path $ProjectRoot "src-tauri\target\release\envnexus-ai.exe"
}
$Executable = [System.IO.Path]::GetFullPath($Executable)
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw "EnvNexus AI executable not found: $Executable"
}

$RunId = [Guid]::NewGuid().ToString("N")
$ArtifactRoot = Join-Path $ProjectRoot "artifacts\smoke\cli-$RunId"
$DataRoot = Join-Path $ArtifactRoot "data"
$PythonRoot = Join-Path $ArtifactRoot "managed-tools\python"
$Snapshot = Join-Path $DataRoot "cache\last-environment-scan.json"
$RootPreferences = Join-Path $DataRoot "config\tool-roots.json"
New-Item -ItemType Directory -Path $ArtifactRoot -Force | Out-Null
$env:ENVNEXUS_AI_DATA_ROOT = $DataRoot

function Invoke-EnvNexusAICli {
    param([string[]]$Arguments)

    $Output = & $Executable @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "EnvNexus AI command engine failed ($($Arguments -join ' ')):`n$($Output -join [Environment]::NewLine)"
    }
    return ($Output -join [Environment]::NewLine)
}

try {
    $ToolsJson = Invoke-EnvNexusAICli -Arguments @("tools", "--json")
    $Tools = $ToolsJson | ConvertFrom-Json
    if (@($Tools).Count -ne 15) {
        throw "Expected 15 built-in tool definitions, got $(@($Tools).Count)."
    }
    if (Test-Path -LiteralPath $Snapshot) {
        throw "The read-only tools command unexpectedly created a scan snapshot."
    }

    $CommandStatusJson = Invoke-EnvNexusAICli -Arguments @("command-scripts", "prepare", "--json")
    $CommandStatus = $CommandStatusJson | ConvertFrom-Json
    if ($CommandStatus.scriptCount -ne 110 -or $CommandStatus.expectedScriptCount -ne 110) {
        throw "Expected 110 generated CMD scripts, got $($CommandStatus.scriptCount)."
    }
    $JdkListScript = Join-Path ([string]$CommandStatus.directory) "jdk-list.cmd"
    $EnvRefreshScript = Join-Path ([string]$CommandStatus.directory) "env-refresh.cmd"
    $EnvRepairScript = Join-Path ([string]$CommandStatus.directory) "env-repair.cmd"
    if (-not (Test-Path -LiteralPath $JdkListScript -PathType Leaf)) {
        throw "jdk-list.cmd was not generated."
    }
    if (-not (Test-Path -LiteralPath $EnvRepairScript -PathType Leaf)) {
        throw "env-repair.cmd was not generated."
    }
    if (-not (Test-Path -LiteralPath $EnvRefreshScript -PathType Leaf)) {
        throw "env-refresh.cmd was not generated."
    }

    $JdkBeforeScanJson = & $JdkListScript "--json" 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "jdk-list.cmd failed before scan: $($JdkBeforeScanJson -join [Environment]::NewLine)"
    }
    $JdkBeforeScanJson = $JdkBeforeScanJson -join [Environment]::NewLine
    $JdkBeforeScan = $JdkBeforeScanJson | ConvertFrom-Json
    if ($JdkBeforeScan.scanFinishedAt) {
        throw "jdk-list unexpectedly reported a scan timestamp."
    }
    if (Test-Path -LiteralPath $Snapshot) {
        throw "jdk-list unexpectedly created a scan snapshot."
    }

    Invoke-EnvNexusAICli -Arguments @("root", "set", "python", $PythonRoot) | Out-Null
    if (-not (Test-Path -LiteralPath $RootPreferences -PathType Leaf)) {
        throw "root set did not persist tool-roots.json."
    }
    $Preferences = Get-Content -LiteralPath $RootPreferences -Raw -Encoding UTF8 | ConvertFrom-Json
    $SavedPythonRoot = [string]$Preferences.roots.python
    if ($SavedPythonRoot.StartsWith('\\?\')) {
        $SavedPythonRoot = $SavedPythonRoot.Substring(4)
    }
    if ($SavedPythonRoot -ne [System.IO.Path]::GetFullPath($PythonRoot)) {
        throw "Saved Python install root does not match the requested path."
    }
    $PythonRootScript = Join-Path ([string]$CommandStatus.directory) "python-root.cmd"
    $PythonRootJson = & $PythonRootScript "get" "--json" 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "python-root.cmd failed after the root changed: $($PythonRootJson -join [Environment]::NewLine)"
    }
    $PythonRootFromGeneratedScript = [string](($PythonRootJson -join [Environment]::NewLine) | ConvertFrom-Json).root
    if ($PythonRootFromGeneratedScript.StartsWith('\\?\')) {
        $PythonRootFromGeneratedScript = $PythonRootFromGeneratedScript.Substring(4)
    }
    if ($PythonRootFromGeneratedScript -ne [System.IO.Path]::GetFullPath($PythonRoot)) {
        throw "The previously generated python-root.cmd did not read the latest saved root."
    }

    Invoke-EnvNexusAICli -Arguments @("scan") | Out-Null
    if (-not (Test-Path -LiteralPath $Snapshot -PathType Leaf)) {
        throw "Explicit scan did not persist the shared snapshot."
    }
    $RefreshOutput = & $EnvRefreshScript "--json" 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "env-refresh.cmd failed: $($RefreshOutput -join [Environment]::NewLine)"
    }
    $Refresh = ($RefreshOutput -join [Environment]::NewLine) | ConvertFrom-Json
    if (@($Refresh.tools).Count -ne 15) {
        throw "env-refresh.cmd returned an incomplete tool inventory."
    }

    $JdkAfterScanJson = & $JdkListScript "--json" 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "jdk-list.cmd failed after scan: $($JdkAfterScanJson -join [Environment]::NewLine)"
    }
    $JdkAfterScanJson = $JdkAfterScanJson -join [Environment]::NewLine
    $JdkAfterScan = $JdkAfterScanJson | ConvertFrom-Json
    if (-not $JdkAfterScan.scanFinishedAt) {
        throw "jdk-list did not reuse the persisted scan timestamp."
    }
    if ($JdkAfterScan.toolId -ne "java") {
        throw "jdk-list alias did not resolve to the Java/JDK plugin."
    }

    $Scan = Get-Content -LiteralPath $Snapshot -Raw -Encoding UTF8 | ConvertFrom-Json
    $AllIssues = @($Scan.issues) + @($Scan.tools | ForEach-Object { $_.issues })
    $RepairableIssue = $AllIssues | Where-Object repairable | Select-Object -First 1
    $PreviewChecked = $false
    if ($RepairableIssue) {
        $UserEnvironmentBefore = Get-ItemProperty -LiteralPath "HKCU:\Environment" |
            Select-Object -Property * -ExcludeProperty PSPath,PSParentPath,PSChildName,PSDrive,PSProvider |
            ConvertTo-Json -Compress
        $PreviewOutput = & $EnvRepairScript ([string]$RepairableIssue.code) "--json" 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw "env-repair.cmd failed to preview a repair: $($PreviewOutput -join [Environment]::NewLine)"
        }
        $PreviewJson = $PreviewOutput -join [Environment]::NewLine
        $Preview = $PreviewJson | ConvertFrom-Json
        if (-not $Preview.confirmationToken) {
            throw "Diagnostic repair preview did not return a confirmation token."
        }
        $UserEnvironmentAfter = Get-ItemProperty -LiteralPath "HKCU:\Environment" |
            Select-Object -Property * -ExcludeProperty PSPath,PSParentPath,PSChildName,PSDrive,PSProvider |
            ConvertTo-Json -Compress
        if ($UserEnvironmentBefore -ne $UserEnvironmentAfter) {
            throw "Preview-only diagnostic repair changed the user environment."
        }
        $PreviewChecked = $true
    }

    $Summary = [ordered]@{
        executable = $Executable
        artifactRoot = $ArtifactRoot
        toolDefinitionCount = @($Tools).Count
        jdkAliasToolId = $JdkAfterScan.toolId
        scanFinishedAt = $JdkAfterScan.scanFinishedAt
        rootPersistenceVerified = $true
        generatedBeforeRootChangeReadsLatestRoot = $true
        readOnlyCommandsDidNotScan = $true
        previewGuardVerified = $PreviewChecked
        generatedCommandCount = $CommandStatus.scriptCount
        generatedJdkListVerified = $true
        generatedEnvRepairVerified = $true
        generatedEnvRefreshVerified = $true
        separateCliExecutable = $false
    }
    $Summary | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $ArtifactRoot "summary.json") -Encoding UTF8
    $Summary | ConvertTo-Json
}
finally {
    Remove-Item Env:ENVNEXUS_AI_DATA_ROOT -ErrorAction SilentlyContinue
}
