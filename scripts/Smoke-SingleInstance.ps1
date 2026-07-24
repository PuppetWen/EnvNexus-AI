param(
    [string]$ExecutablePath = "",
    [int]$StartupTimeoutSeconds = 30,
    [int]$SecondInstanceTimeoutSeconds = 10
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$Executable = if ($ExecutablePath) {
    [System.IO.Path]::GetFullPath($ExecutablePath)
}
else {
    Join-Path $ProjectRoot "src-tauri\target\release\envnexus-ai.exe"
}
$RunId = [Guid]::NewGuid().ToString("N")
$ArtifactRoot = Join-Path $ProjectRoot "artifacts\smoke\single-instance-$RunId"
$DataRoot = Join-Path $ArtifactRoot "data"
$WebViewDataRoot = Join-Path $ArtifactRoot "webview2"
$Snapshot = Join-Path $DataRoot "cache\last-environment-scan.json"

if (-not (Test-Path -LiteralPath $Executable)) {
    throw "Release executable not found. Run scripts\Invoke-Rust.ps1 -Task tauri first."
}

New-Item -ItemType Directory -Path $ArtifactRoot -Force | Out-Null

if (-not ("EnvNexusAISingleInstanceSmoke" -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EnvNexusAISingleInstanceSmoke {
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int command);
}
'@
}

function Wait-ForMainWindow {
    param([System.Diagnostics.Process]$Process)

    $Deadline = [DateTime]::UtcNow.AddSeconds($StartupTimeoutSeconds)
    do {
        Start-Sleep -Milliseconds 200
        $Process.Refresh()
        if ($Process.HasExited) {
            throw "The first EnvNexus AI instance exited during startup with code $($Process.ExitCode)."
        }
        if ($Process.MainWindowHandle -ne 0) {
            return
        }
    } while ([DateTime]::UtcNow -lt $Deadline)

    throw "The first EnvNexus AI instance did not create a main window."
}

$PreviousDataRoot = $env:ENVNEXUS_AI_DATA_ROOT
$PreviousWebViewDataRoot = $env:WEBVIEW2_USER_DATA_FOLDER
$FirstProcess = $null
$SecondProcess = $null

try {
    $env:ENVNEXUS_AI_DATA_ROOT = $DataRoot
    $env:WEBVIEW2_USER_DATA_FOLDER = $WebViewDataRoot

    $FirstProcess = Start-Process -FilePath $Executable -PassThru -WindowStyle Hidden
    Wait-ForMainWindow -Process $FirstProcess

    $CliJson = & $Executable tools --json | Out-String
    $CliExitCode = $LASTEXITCODE
    $CliTools = $CliJson | ConvertFrom-Json
    if ($CliExitCode -ne 0 -or @($CliTools).Count -ne 15) {
        throw "Command mode was incorrectly intercepted by the running GUI instance."
    }

    [EnvNexusAISingleInstanceSmoke]::ShowWindow($FirstProcess.MainWindowHandle, 0) | Out-Null
    Start-Sleep -Milliseconds 500
    if ([EnvNexusAISingleInstanceSmoke]::IsWindowVisible($FirstProcess.MainWindowHandle)) {
        throw "Unable to hide the first window before the second-launch check."
    }

    $SecondProcess = Start-Process -FilePath $Executable -PassThru -WindowStyle Hidden
    if (-not $SecondProcess.WaitForExit($SecondInstanceTimeoutSeconds * 1000)) {
        throw "The second EnvNexus AI process remained alive; single-instance enforcement failed."
    }

    $FirstProcess.Refresh()
    if ($FirstProcess.HasExited) {
        throw "The original EnvNexus AI process exited when the second copy was launched."
    }

    $VisibleDeadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        Start-Sleep -Milliseconds 200
        $FirstProcess.Refresh()
        $IsVisible = [EnvNexusAISingleInstanceSmoke]::IsWindowVisible($FirstProcess.MainWindowHandle)
    } while (-not $IsVisible -and [DateTime]::UtcNow -lt $VisibleDeadline)

    if (-not $IsVisible) {
        throw "The second launch exited, but it did not restore the existing EnvNexus AI window."
    }

    [pscustomobject]@{
        FirstProcessStayedAlive = $true
        SecondProcessExited = $true
        SecondProcessExitCode = $SecondProcess.ExitCode
        ExistingWindowRestored = $true
        CommandModeToolCount = @($CliTools).Count
        ScanSnapshotCreated = Test-Path -LiteralPath $Snapshot
        ArtifactRoot = $ArtifactRoot
    } | Format-List
}
finally {
    if ($SecondProcess -and -not $SecondProcess.HasExited) {
        Stop-Process -Id $SecondProcess.Id -Force
    }
    if ($FirstProcess -and -not $FirstProcess.HasExited) {
        Stop-Process -Id $FirstProcess.Id -Force
    }
    $env:ENVNEXUS_AI_DATA_ROOT = $PreviousDataRoot
    $env:WEBVIEW2_USER_DATA_FOLDER = $PreviousWebViewDataRoot
}
