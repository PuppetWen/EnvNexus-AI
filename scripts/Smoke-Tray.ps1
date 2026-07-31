param(
    [string]$Executable = "",
    [int]$StartupWaitSeconds = 3
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
if (-not $Executable) {
    $Executable = Join-Path $ProjectRoot "src-tauri\target\release\envnexus-ai.exe"
}
$Executable = [System.IO.Path]::GetFullPath($Executable)
$CdpHelper = Join-Path $ProjectRoot "scripts\webview-cdp.mjs"
$RunId = [Guid]::NewGuid().ToString("N")
$ArtifactRoot = Join-Path $ProjectRoot "artifacts\smoke\tray-$RunId"
$DataRoot = Join-Path $ArtifactRoot "data"
$Snapshot = Join-Path $DataRoot "cache\last-environment-scan.json"
$PreferencesPath = Join-Path $DataRoot "config\app-preferences.json"
$WebViewDataRoot = Join-Path $ArtifactRoot "webview2"

if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw "Release executable not found: $Executable"
}
if (-not (Test-Path -LiteralPath $CdpHelper -PathType Leaf)) {
    throw "WebView CDP helper not found."
}

New-Item -ItemType Directory -Path $ArtifactRoot -Force | Out-Null
$env:ENVNEXUS_AI_DATA_ROOT = $DataRoot
$env:WEBVIEW2_USER_DATA_FOLDER = $WebViewDataRoot

Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class EnvNexusAITraySmoke {
  public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int count);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint message, IntPtr wParam, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int command);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);

  public static IntPtr FindMainWindow(int targetProcessId) {
    IntPtr result = IntPtr.Zero;
    EnumWindows((hWnd, lParam) => {
      uint processId;
      GetWindowThreadProcessId(hWnd, out processId);
      if (processId != (uint)targetProcessId) return true;
      var text = new StringBuilder(256);
      GetWindowText(hWnd, text, text.Capacity);
      if (text.ToString() == "EnvNexus AI") {
        result = hWnd;
        return false;
      }
      return true;
    }, IntPtr.Zero);
    return result;
  }
}
'@

function Wait-ForCdp {
    param([int]$Port)
    $Deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        try {
            $Targets = Invoke-RestMethod -TimeoutSec 2 -Uri "http://127.0.0.1:$Port/json/list"
            if ($Targets | Where-Object webSocketDebuggerUrl) {
                return
            }
        }
        catch {
            Start-Sleep -Milliseconds 250
        }
    } while ([DateTime]::UtcNow -lt $Deadline)
    throw "EnvNexus AI WebView CDP target did not become available on port $Port."
}

function Get-FreeTcpPort {
    $Listener = [System.Net.Sockets.TcpListener]::new(
        [System.Net.IPAddress]::Loopback,
        0
    )
    $Listener.Start()
    try {
        return ([System.Net.IPEndPoint]$Listener.LocalEndpoint).Port
    }
    finally {
        $Listener.Stop()
    }
}

function Wait-ForWindow {
    param([System.Diagnostics.Process]$Process)
    $Deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 250
        $Process.Refresh()
        if ($Process.HasExited) {
            throw "EnvNexus AI exited during startup with code $($Process.ExitCode)."
        }
        $Handle = [EnvNexusAITraySmoke]::FindMainWindow($Process.Id)
        if ($Handle -ne [IntPtr]::Zero) {
            return $Handle
        }
    } while ([DateTime]::UtcNow -lt $Deadline)
    throw "EnvNexus AI did not create its main window."
}

function Invoke-Cdp {
    param(
        [int]$Port,
        [string]$Expression
    )
    $Output = & node $CdpHelper $Port $Expression
    if ($LASTEXITCODE -ne 0) {
        throw "EnvNexus AI WebView CDP expression failed: $Expression"
    }
    return $Output
}

function Wait-ForVisibility {
    param(
        [IntPtr]$Handle,
        [bool]$Visible,
        [string]$Stage = "unknown"
    )
    $Deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        if ([EnvNexusAITraySmoke]::IsWindowVisible($Handle) -eq $Visible) {
            return
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $Deadline)
    $IsIconic = [EnvNexusAITraySmoke]::IsIconic($Handle)
    throw "Window visibility did not become '$Visible' during '$Stage' (IsIconic=$IsIconic)."
}

function Show-MainWindow {
    param(
        [IntPtr]$Handle,
        [int]$Port
    )
    Invoke-Cdp -Port $Port -Expression "window.__TAURI_INTERNALS__.invoke('restore_main_window')" | Out-Null
    Wait-ForVisibility -Handle $Handle -Visible $true -Stage "restore"
}

$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$RunValueName = "EnvNexus AI"
$OriginalRunValueExists = $false
$OriginalRunValue = $null
try {
    $OriginalRunValue = Get-ItemPropertyValue -LiteralPath $RunKey -Name $RunValueName -ErrorAction Stop
    $OriginalRunValueExists = $true
}
catch {
    $OriginalRunValueExists = $false
}

try {
    $CdpPort = Get-FreeTcpPort
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$CdpPort"
    $FirstProcess = Start-Process -FilePath $Executable -PassThru
    try {
        $FirstHandle = Wait-ForWindow -Process $FirstProcess
        Wait-ForCdp -Port $CdpPort
        $UiDeadline = [DateTime]::UtcNow.AddSeconds(10)
        do {
            $ShellReady = Invoke-Cdp -Port $CdpPort -Expression "Boolean(document.querySelector('.app-shell'))"
            if ($ShellReady -eq "true") {
                break
            }
            Start-Sleep -Milliseconds 200
        } while ([DateTime]::UtcNow -lt $UiDeadline)
        Invoke-Cdp -Port $CdpPort -Expression "window.__TAURI_INTERNALS__.invoke('plugin:event|emit',{event:'tray-action',payload:{kind:'openTool',toolId:'python'}})" | Out-Null
        Start-Sleep -Seconds 2
        $PythonToolOpened = Invoke-Cdp -Port $CdpPort -Expression "Boolean(document.querySelector('.tool-detail-hero h1')?.textContent==='Python' && document.querySelector('[data-tool-root-input=""python""]') && !document.querySelector('.empty-state'))"
        if ($PythonToolOpened -ne "true") {
            $PageState = Invoke-Cdp -Port $CdpPort -Expression "JSON.stringify({breadcrumb:document.querySelector('.breadcrumb')?.innerText,body:document.body?.innerText?.slice(0,800)})"
            throw "Tray open-tool action did not enter the Python management page: $PageState"
        }
        Start-Sleep -Seconds $StartupWaitSeconds
        if (Test-Path -LiteralPath $Snapshot) {
            throw "Startup created a scan snapshot without a user action."
        }

        Invoke-Cdp -Port $CdpPort -Expression "document.querySelector('.primary-nav [data-nav=""settings""]')?.click(); true" | Out-Null
        Start-Sleep -Seconds 1
        $ControlsReady = Invoke-Cdp -Port $CdpPort -Expression "Boolean(document.querySelector('#app-close-behavior') && document.querySelector('#app-start-minimized') && document.querySelector('#app-launch-at-login') && document.querySelector('#app-language') && !document.querySelector('#app-minimize-button-to-tray') && document.querySelector('.tray-capabilities strong'))"
        if ($ControlsReady -ne "true") {
            $PageState = Invoke-Cdp -Port $CdpPort -Expression "JSON.stringify({title:document.title,text:document.body?.innerText?.slice(0,500),settings:Boolean(document.querySelector('.primary-nav [data-nav=""settings""]')),closeBehavior:Boolean(document.querySelector('#app-close-behavior'))})"
            throw "Application behavior controls or tray readiness status are missing. Page state: $PageState"
        }

        $TrayStatus = Invoke-Cdp -Port $CdpPort -Expression "window.__TAURI_INTERNALS__.invoke('tray_menu_status')"
        if ($TrayStatus -notmatch '"ready":true' -or $TrayStatus -notmatch '"toolEntries":15') {
            throw "Tray tool hierarchy was not initialized: $TrayStatus"
        }

        Invoke-Cdp -Port $CdpPort -Expression "(()=>{document.querySelector('#app-close-behavior').value='minimizeToTray'; document.querySelector('#app-start-minimized').checked=false; document.querySelector('#app-launch-at-login').checked=true; document.querySelector('#app-language').value='en-US'; document.querySelector('#save-app-preferences').click(); return true;})()" | Out-Null
        Start-Sleep -Seconds 2
        if (-not (Test-Path -LiteralPath $PreferencesPath -PathType Leaf)) {
            throw "Application behavior settings were not persisted."
        }
        $Saved = Get-Content -LiteralPath $PreferencesPath -Raw -Encoding UTF8 | ConvertFrom-Json
        if (
            $Saved.closeBehavior -ne "minimizeToTray" -or
            $Saved.startMinimized -ne $false -or
            $Saved.launchAtLogin -ne $true -or
            $Saved.language -ne "en-US"
        ) {
            throw "Persisted application behavior settings do not match the UI selection."
        }
        $ExpectedRunValue = '"' + $Executable + '"'
        $RunValue = Get-ItemPropertyValue -LiteralPath $RunKey -Name $RunValueName -ErrorAction Stop
        if ($RunValue -ne $ExpectedRunValue) {
            throw "Windows startup entry does not point to the current EnvNexus AI executable."
        }
        $LocalizedSettings = Invoke-Cdp -Port $CdpPort -Expression "document.documentElement.lang + ':' + document.querySelector('.primary-nav [data-nav=""settings""] span')?.textContent"
        if ($LocalizedSettings -ne '"en-US:Settings"') {
            throw "English interface language did not apply: $LocalizedSettings"
        }

        [EnvNexusAITraySmoke]::PostMessage($FirstHandle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        Wait-ForVisibility -Handle $FirstHandle -Visible $false -Stage "close button to tray"
        $FirstProcess.Refresh()
        if ($FirstProcess.HasExited) {
            throw "Close-to-tray setting exited the process instead of hiding the window."
        }
        Show-MainWindow -Handle $FirstHandle -Port $CdpPort
        $SettingsPreservedAfterClose = Invoke-Cdp -Port $CdpPort -Expression "Boolean(document.querySelector('#app-close-behavior') && document.querySelector('.primary-nav [data-nav=""settings""]')?.classList.contains('active'))"
        if ($SettingsPreservedAfterClose -ne "true") {
            throw "Restoring the window after close-to-tray did not preserve the current settings view."
        }

        Invoke-Cdp -Port $CdpPort -Expression "document.querySelector('#hide-to-tray')?.click(); true" | Out-Null
        Wait-ForVisibility -Handle $FirstHandle -Visible $false -Stage "hide now button"
        Show-MainWindow -Handle $FirstHandle -Port $CdpPort
        $SettingsPreservedAfterHide = Invoke-Cdp -Port $CdpPort -Expression "Boolean(document.querySelector('#app-close-behavior') && document.querySelector('.primary-nav [data-nav=""settings""]')?.classList.contains('active'))"
        if ($SettingsPreservedAfterHide -ne "true") {
            throw "Restoring the window after an explicit hide did not preserve the current settings view."
        }

        Invoke-Cdp -Port $CdpPort -Expression "(()=>{document.querySelector('#app-close-behavior').value='minimizeToTray'; document.querySelector('#app-start-minimized').checked=true; document.querySelector('#app-launch-at-login').checked=false; document.querySelector('#app-language').value='ja-JP'; document.querySelector('#save-app-preferences').click(); return true;})()" | Out-Null
        Start-Sleep -Seconds 2
        $StartupEntryRemoved = $false
        try {
            Get-ItemPropertyValue -LiteralPath $RunKey -Name $RunValueName -ErrorAction Stop | Out-Null
        }
        catch {
            $StartupEntryRemoved = $true
        }
        if (-not $StartupEntryRemoved) {
            throw "Windows startup entry remained after disabling launch at login."
        }
    }
    finally {
        if (-not $FirstProcess.HasExited) {
            Stop-Process -Id $FirstProcess.Id -Force
            $FirstProcess.WaitForExit()
        }
    }

    $CdpPort = Get-FreeTcpPort
    $env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $ArtifactRoot "webview2-restart"
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$CdpPort"
    $SecondProcess = Start-Process -FilePath $Executable -PassThru
    try {
        Wait-ForCdp -Port $CdpPort
        Start-Sleep -Seconds $StartupWaitSeconds
        $SecondProcess.Refresh()
        if ($SecondProcess.HasExited) {
            throw "Start-minimized setting unexpectedly exited the process."
        }
        $SecondHandle = [EnvNexusAITraySmoke]::FindMainWindow($SecondProcess.Id)
        if ($SecondHandle -eq [IntPtr]::Zero) {
            throw "The hidden EnvNexus AI main window could not be located."
        }
        if ([EnvNexusAITraySmoke]::IsWindowVisible($SecondHandle)) {
            throw "Start-minimized setting left the main window visible."
        }
        if (Test-Path -LiteralPath $Snapshot) {
            throw "Starting minimized triggered an environment scan."
        }

        Show-MainWindow -Handle $SecondHandle -Port $CdpPort
        Invoke-Cdp -Port $CdpPort -Expression "document.querySelector('.primary-nav [data-nav=""settings""]')?.click(); true" | Out-Null
        Start-Sleep -Seconds 1
        Invoke-Cdp -Port $CdpPort -Expression "(()=>{document.querySelector('#app-close-behavior').value='exit'; document.querySelector('#app-start-minimized').checked=false; document.querySelector('#app-launch-at-login').checked=false; document.querySelector('#app-language').value='zh-CN'; document.querySelector('#save-app-preferences').click(); return true;})()" | Out-Null
        Start-Sleep -Seconds 2
        [EnvNexusAITraySmoke]::PostMessage($SecondHandle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        if (-not $SecondProcess.WaitForExit(10000)) {
            throw "Exit close behavior did not terminate EnvNexus AI."
        }
    }
    finally {
        if (-not $SecondProcess.HasExited) {
            Stop-Process -Id $SecondProcess.Id -Force
        }
    }

    [pscustomobject]@{
        Executable = $Executable
        ArtifactRoot = $ArtifactRoot
        TrayReadyReported = $true
        TrayToolHierarchyVerified = $true
        TrayToolOpenActionVerified = $true
        CloseButtonToTrayVerified = $true
        HideNowButtonVerified = $true
        StartMinimizedVerified = $true
        LaunchAtLoginVerified = $true
        LanguageSwitchVerified = $true
        ExitCloseBehaviorVerified = $true
        StartupDidNotScan = $true
        PreferencesPath = $PreferencesPath
    } | Format-List
}
finally {
    New-Item -ItemType Directory -Path $RunKey -Force | Out-Null
    if ($OriginalRunValueExists) {
        Set-ItemProperty -LiteralPath $RunKey -Name $RunValueName -Value $OriginalRunValue
    }
    else {
        Remove-ItemProperty -LiteralPath $RunKey -Name $RunValueName -ErrorAction SilentlyContinue
    }
}
