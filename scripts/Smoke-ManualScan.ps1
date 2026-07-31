param(
    [int]$StartupWaitSeconds = 8,
    [int]$ScanTimeoutSeconds = 120
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$Executable = Join-Path $ProjectRoot "src-tauri\target\release\envnexus-ai.exe"
$CdpHelper = Join-Path $ProjectRoot "scripts\webview-cdp.mjs"
$CdpPort = Get-Random -Minimum 12000 -Maximum 18000
$RunId = [Guid]::NewGuid().ToString("N")
$ArtifactRoot = Join-Path $ProjectRoot "artifacts\smoke\manual-scan-$RunId"
$DataRoot = Join-Path $ArtifactRoot "data"
$TypedPythonRoot = Join-Path $ArtifactRoot "managed-tools\python"
$CustomCommandDirectory = Join-Path $ArtifactRoot "terminal-command-scripts"
$Snapshot = Join-Path $DataRoot "cache\last-environment-scan.json"
$BeforeScanScreenshot = Join-Path $ArtifactRoot "01-before-manual-scan.png"
$UnscannedToolsScreenshot = Join-Path $ArtifactRoot "02-unscanned-tools.png"
$UnscannedPythonScreenshot = Join-Path $ArtifactRoot "03-unscanned-python-management.png"
$AfterScanScreenshot = Join-Path $ArtifactRoot "04-after-manual-scan.png"
$RestartScreenshot = Join-Path $ArtifactRoot "05-restart-reuses-snapshot.png"
$DiagnosticsScreenshot = Join-Path $ArtifactRoot "06-diagnostics.png"
$LocalGuidanceScreenshot = Join-Path $ArtifactRoot "07-local-diagnostic-guidance.png"
$RepairPlanScreenshot = Join-Path $ArtifactRoot "07-diagnostic-repair-plan.png"
$CommandsScreenshot = Join-Path $ArtifactRoot "08-tool-commands.png"
$SettingsScreenshot = Join-Path $ArtifactRoot "09-ai-settings.png"
$AiProviderScreenshot = Join-Path $ArtifactRoot "10-ai-provider-config.png"
$GameHudScreenshot = Join-Path $ArtifactRoot "11-game-hud-dashboard.png"
$WebViewDataRoot = Join-Path $ArtifactRoot "webview2"

if (-not (Test-Path -LiteralPath $Executable)) {
    throw "Release executable not found. Run scripts\Invoke-Rust.ps1 -Task tauri first."
}
if (-not (Test-Path -LiteralPath $CdpHelper)) {
    throw "WebView CDP helper not found."
}

New-Item -ItemType Directory -Path $ArtifactRoot -Force | Out-Null
$env:ENVNEXUS_AI_DATA_ROOT = $DataRoot
$env:WEBVIEW2_USER_DATA_FOLDER = $WebViewDataRoot
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$CdpPort"

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EnvNexusAIManualScanSmoke {
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int command);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint flags);
}
'@

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

function Wait-ForMainWindow {
    param([System.Diagnostics.Process]$Process)

    $Deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 250
        $Process.Refresh()
        if ($Process.HasExited) {
            throw "EnvNexus AI exited during startup with code $($Process.ExitCode)."
        }
        if ($Process.MainWindowHandle -ne 0) {
            return
        }
    } while ([DateTime]::UtcNow -lt $Deadline)

    throw "EnvNexus AI did not create a main window."
}

function Get-WindowBounds {
    param([System.Diagnostics.Process]$Process)

    [EnvNexusAIManualScanSmoke]::ShowWindow($Process.MainWindowHandle, 3) | Out-Null
    [EnvNexusAIManualScanSmoke]::SetForegroundWindow($Process.MainWindowHandle) | Out-Null
    Start-Sleep -Seconds 1
    $Bounds = New-Object EnvNexusAIManualScanSmoke+RECT
    if (-not [EnvNexusAIManualScanSmoke]::GetWindowRect($Process.MainWindowHandle, [ref]$Bounds)) {
        throw "EnvNexus AI window bounds could not be read."
    }
    return $Bounds
}

function Save-WindowCapture {
    param(
        [System.Diagnostics.Process]$Process,
        [string]$Path
    )

    $Bounds = Get-WindowBounds -Process $Process
    $Width = $Bounds.Right - $Bounds.Left
    $Height = $Bounds.Bottom - $Bounds.Top
    if ($Width -le 0 -or $Height -le 0) {
        throw "EnvNexus AI returned invalid window bounds."
    }

    $Bitmap = New-Object System.Drawing.Bitmap $Width, $Height
    $Graphics = [System.Drawing.Graphics]::FromImage($Bitmap)
    try {
        $Hdc = $Graphics.GetHdc()
        try {
            if (-not [EnvNexusAIManualScanSmoke]::PrintWindow($Process.MainWindowHandle, $Hdc, 2)) {
                throw "EnvNexus AI window capture failed."
            }
        }
        finally {
            $Graphics.ReleaseHdc($Hdc)
        }
        $Bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $Graphics.Dispose()
        $Bitmap.Dispose()
    }
}

function Wait-ForCdp {
    param(
        [int]$Port
    )

    $Deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        try {
            $Targets = Invoke-RestMethod -TimeoutSec 2 -Uri "http://127.0.0.1:$Port/json/list"
            if ($Targets | Where-Object title -eq "EnvNexus AI") {
                return
            }
        }
        catch {
            Start-Sleep -Milliseconds 250
        }
    } while ([DateTime]::UtcNow -lt $Deadline)

    throw "EnvNexus AI WebView CDP target did not become available on port $Port."
}

function Invoke-CdpExpression {
    param(
        [int]$Port,
        [string]$Expression
    )

    & node $CdpHelper $Port $Expression | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "EnvNexus AI WebView CDP expression failed: $Expression"
    }
}

[EnvNexusAIManualScanSmoke]::SetProcessDPIAware() | Out-Null

$FirstProcess = Start-Process -FilePath $Executable -PassThru
try {
    Wait-ForMainWindow -Process $FirstProcess
    Wait-ForCdp -Port $CdpPort
    Start-Sleep -Seconds $StartupWaitSeconds
    if (Test-Path -LiteralPath $Snapshot) {
        throw "Fresh startup created a scan snapshot without a user click."
    }
    Save-WindowCapture -Process $FirstProcess -Path $BeforeScanScreenshot

    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('[data-nav=""tools""]')?.click(); true"
    Start-Sleep -Seconds 1
    $UnscannedToolCount = & node $CdpHelper $CdpPort "document.querySelectorAll('[data-tool-card]').length"
    $CoreToolCount = & node $CdpHelper $CdpPort "document.querySelectorAll('[data-tool-group=""core""] [data-tool-card]').length"
    $AndroidToolCount = & node $CdpHelper $CdpPort "document.querySelectorAll('[data-tool-group=""android""] [data-tool-card]').length"
    $AndroidPathInputVisible = & node $CdpHelper $CdpPort "Boolean(document.querySelector('[data-android-root-input]'))"
    $StandaloneAndroidNavigation = & node $CdpHelper $CdpPort "Boolean(document.querySelector('[data-nav=""android""]'))"
    if (
        $LASTEXITCODE -ne 0 -or
        $UnscannedToolCount -ne "15" -or
        $CoreToolCount -ne "9" -or
        $AndroidToolCount -ne "6" -or
        $AndroidPathInputVisible -ne "true" -or
        $StandaloneAndroidNavigation -ne "false"
    ) {
        throw "The unscanned tool library did not expose 9 common tools, 6 Android tools, and the shared Android path input."
    }
    Save-WindowCapture -Process $FirstProcess -Path $UnscannedToolsScreenshot

    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('[data-open-tool=""python""]')?.click(); true"
    Start-Sleep -Seconds 1
    $TypedPythonRootBase64 = [Convert]::ToBase64String(
        [Text.Encoding]::UTF8.GetBytes($TypedPythonRoot)
    )
    Invoke-CdpExpression -Port $CdpPort -Expression "const input=document.querySelector('[data-tool-root-input=""python""]'); input.value=new TextDecoder().decode(Uint8Array.from(atob('$TypedPythonRootBase64'), character=>character.charCodeAt(0))); document.querySelector('[data-save-tool-root=""python""]')?.click(); true"
    Start-Sleep -Seconds 2
    $ToolRootConfig = Join-Path $DataRoot "config\tool-roots.json"
    if (-not (Test-Path -LiteralPath $ToolRootConfig)) {
        throw "Typing and saving a Python root did not persist tool-roots.json."
    }
    $SavedRoots = Get-Content -LiteralPath $ToolRootConfig -Raw -Encoding UTF8 | ConvertFrom-Json
    $SavedPythonRoot = [string]$SavedRoots.roots.python
    if ($SavedPythonRoot.StartsWith('\\?\')) {
        $SavedPythonRoot = $SavedPythonRoot.Substring(4)
    }
    if ($SavedPythonRoot -ne $TypedPythonRoot) {
        throw "The typed Python root was not persisted exactly."
    }
    Save-WindowCapture -Process $FirstProcess -Path $UnscannedPythonScreenshot

    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('#scan-button')?.click(); true"

    $ScanDeadline = [DateTime]::UtcNow.AddSeconds($ScanTimeoutSeconds)
    while (-not (Test-Path -LiteralPath $Snapshot) -and [DateTime]::UtcNow -lt $ScanDeadline) {
        Start-Sleep -Milliseconds 500
    }
    if (-not (Test-Path -LiteralPath $Snapshot)) {
        throw "Manual scan did not persist a snapshot within $ScanTimeoutSeconds seconds."
    }
    Start-Sleep -Seconds 2
    $TrayMenuStatusJson = & node $CdpHelper $CdpPort "window.__TAURI_INTERNALS__.invoke('tray_menu_status')"
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to read the refreshed tray tool hierarchy."
    }
    $TrayMenuStatus = $TrayMenuStatusJson | ConvertFrom-Json
    $TraySnapshot = Get-Content -LiteralPath $Snapshot -Raw -Encoding UTF8 | ConvertFrom-Json
    $ExpectedTraySwitchEntries = @(
        $TraySnapshot.tools |
            ForEach-Object { $_.installedVersions } |
            Where-Object { -not $_.isDefault }
    ).Count
    $TrayDiagnosticIssues = @(
        @($TraySnapshot.issues) +
        @($TraySnapshot.tools | ForEach-Object { $_.issues })
    )
    $ExpectedTrayDiagnosticEntries = $TrayDiagnosticIssues.Count
    $ExpectedTrayDiagnosticRepairEntries = @(
        $TrayDiagnosticIssues | Where-Object { $_.repairable }
    ).Count
    if (
        $TrayMenuStatus.ready -ne $true -or
        $TrayMenuStatus.toolEntries -ne 15 -or
        $TrayMenuStatus.switchEntries -ne $ExpectedTraySwitchEntries -or
        $TrayMenuStatus.diagnosticEntries -ne $ExpectedTrayDiagnosticEntries -or
        $TrayMenuStatus.diagnosticRepairEntries -ne $ExpectedTrayDiagnosticRepairEntries
    ) {
        throw "Tray hierarchy does not match the cached scan snapshot."
    }
    Save-WindowCapture -Process $FirstProcess -Path $AfterScanScreenshot

    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('[data-nav=""diagnostics""]')?.click(); true"
    Start-Sleep -Seconds 2
    Save-WindowCapture -Process $FirstProcess -Path $DiagnosticsScreenshot

    Invoke-CdpExpression -Port $CdpPort -Expression "(()=>{const target=document.querySelector('[data-local-guidance]'); const issueCode=decodeURIComponent(target?.dataset.localGuidance??''); return window.__TAURI_INTERNALS__.invoke('plugin:event|emit',{event:'tray-action',payload:{kind:'openDiagnostic',issueCode}});})()"
    Start-Sleep -Seconds 2
    $GuidanceVisible = & node $CdpHelper $CdpPort "Boolean(document.querySelector('.diagnostic-guidance-modal'))"
    $GuidanceCommandCount = & node $CdpHelper $CdpPort "document.querySelectorAll('[data-copy-guidance-command]').length"
    if (
        $LASTEXITCODE -ne 0 -or
        $GuidanceVisible -ne "true" -or
        [int]$GuidanceCommandCount -lt 1
    ) {
        throw "Local diagnostic guidance did not expose analysis and copyable commands."
    }
    Save-WindowCapture -Process $FirstProcess -Path $LocalGuidanceScreenshot
    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('#close-diagnostic-guidance')?.click(); true"

    Invoke-CdpExpression -Port $CdpPort -Expression "(()=>{const target=document.querySelector('[data-repair-issue]'); const issueCode=decodeURIComponent(target?.dataset.repairIssue??''); return window.__TAURI_INTERNALS__.invoke('plugin:event|emit',{event:'tray-action',payload:{kind:'previewDiagnosticRepair',issueCode}});})()"
    Start-Sleep -Seconds 2
    $PlanVisible = & node $CdpHelper $CdpPort "Boolean(document.querySelector('.plan-modal'))"
    if ($LASTEXITCODE -ne 0 -or $PlanVisible -ne "true") {
        throw "Clicking a repairable diagnostic did not open a repair plan."
    }
    Save-WindowCapture -Process $FirstProcess -Path $RepairPlanScreenshot
    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('#cancel-plan')?.click(); true"

    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('[data-nav=""commands""]')?.click(); true"
    Start-Sleep -Seconds 2
    $CommandToolCount = & node $CdpHelper $CdpPort "document.querySelectorAll('[data-command-tool]').length"
    if ($LASTEXITCODE -ne 0 -or $CommandToolCount -ne "15") {
        throw "The command help page did not expose all 15 tool command groups."
    }
    $CustomCommandDirectoryBase64 = [Convert]::ToBase64String(
        [Text.Encoding]::UTF8.GetBytes($CustomCommandDirectory)
    )
    Invoke-CdpExpression -Port $CdpPort -Expression "(()=>{const commandDirectoryInput=document.querySelector('#terminal-command-directory'); commandDirectoryInput.value=new TextDecoder().decode(Uint8Array.from(atob('$CustomCommandDirectoryBase64'), character=>character.charCodeAt(0))); document.querySelector('#save-terminal-command-directory')?.click(); return true;})()"
    Start-Sleep -Seconds 2
    $TerminalCommandConfig = Join-Path $DataRoot "config\terminal-commands.json"
    if (-not (Test-Path -LiteralPath $TerminalCommandConfig -PathType Leaf)) {
        throw "The typed command script directory was not persisted."
    }
    $SavedCommandSettings = Get-Content -LiteralPath $TerminalCommandConfig -Raw -Encoding UTF8 | ConvertFrom-Json
    $SavedCommandDirectory = [string]$SavedCommandSettings.directory
    if ($SavedCommandDirectory.StartsWith('\\?\')) {
        $SavedCommandDirectory = $SavedCommandDirectory.Substring(4)
    }
    if ($SavedCommandDirectory -ne [System.IO.Path]::GetFullPath($CustomCommandDirectory)) {
        throw "The custom command script directory was not saved exactly."
    }
    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('#enable-terminal-commands')?.click(); true"
    Start-Sleep -Seconds 2
    $CommandPlanVisible = & node $CdpHelper $CdpPort "Boolean(document.querySelector('.plan-modal'))"
    if ($LASTEXITCODE -ne 0 -or $CommandPlanVisible -ne "true") {
        throw "Enabling terminal commands did not open the user PATH preview plan."
    }
    $GeneratedCommandCount = @(Get-ChildItem -LiteralPath $CustomCommandDirectory -Filter "*.cmd").Count
    if ($GeneratedCommandCount -ne 110) {
        throw "Expected 110 generated command scripts, got $GeneratedCommandCount."
    }
    Save-WindowCapture -Process $FirstProcess -Path $CommandsScreenshot
    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('#cancel-plan')?.click(); true"

    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('[data-nav=""settings""]')?.click(); true"
    Start-Sleep -Seconds 2
    Invoke-CdpExpression -Port $CdpPort -Expression "document.documentElement.dataset.theme='modern-tech'; true"
    $AiBrandIconCount = & node $CdpHelper $CdpPort "document.querySelectorAll('.ai-provider-tabs .ai-brand-icon').length"
    $ModernRoundAiIcon = & node $CdpHelper $CdpPort "getComputedStyle(document.querySelector('.ai-provider-brand')).borderRadius==='50%'"
    $OtherThemeKeepsOwnIconShape = & node $CdpHelper $CdpPort "(()=>{document.documentElement.dataset.theme='cyberpunk';const isolated=getComputedStyle(document.querySelector('.ai-provider-brand')).borderRadius!=='50%';document.documentElement.dataset.theme='modern-tech';return isolated})()"
    if (
        $LASTEXITCODE -ne 0 -or
        [int]$AiBrandIconCount -ne 9 -or
        $ModernRoundAiIcon -ne "true" -or
        $OtherThemeKeepsOwnIconShape -ne "true"
    ) {
        throw "AI provider icons or modern-tech-only circular HUD styling did not render as expected."
    }
    $ButtonLayoutAligned = & node $CdpHelper $CdpPort "(()=>{const first=document.querySelector('#hide-to-tray');const second=document.querySelector('#save-app-preferences');if(!first||!second)return false;const a=first.getBoundingClientRect();const b=second.getBoundingClientRect();const style=getComputedStyle(first);return style.display==='flex'||style.display==='inline-flex'?style.alignItems==='center'&&Math.abs((a.top+a.height/2)-(b.top+b.height/2))<1&&Math.abs(a.height-b.height)<1:false})()"
    if ($LASTEXITCODE -ne 0 -or $ButtonLayoutAligned -ne "true") {
        throw "Settings action button icons and labels are not aligned on one centered row."
    }
    $ScrollBeforeRender = & node $CdpHelper $CdpPort "(()=>{const content=document.querySelector('.content');const target=document.querySelector('[data-ai-provider=""deepseek""]');if(!content||!target)return -1;target.scrollIntoView({block:'center'});const before=content.scrollTop;target.click();return before})()"
    Start-Sleep -Milliseconds 500
    $ScrollAfterRender = & node $CdpHelper $CdpPort "document.querySelector('.content')?.scrollTop??-1"
    if (
        $LASTEXITCODE -ne 0 -or
        [double]$ScrollBeforeRender -le 0 -or
        [Math]::Abs([double]$ScrollAfterRender - [double]$ScrollBeforeRender) -gt 2
    ) {
        throw "A settings button re-render reset the main content scroll position."
    }

    Invoke-CdpExpression -Port $CdpPort -Expression "window.__TAURI_INTERNALS__.invoke('save_ai_provider',{input:{id:'openai',displayName:'OpenAI',protocol:'openai',baseUrl:'https://api.openai.com/v1',selectedModel:'envpilot-openai-test-model',apiKey:'envpilot-not-a-real-openai-key'}})"
    Invoke-CdpExpression -Port $CdpPort -Expression "window.__TAURI_INTERNALS__.invoke('save_ai_provider',{input:{id:'deepseek',displayName:'DeepSeek',protocol:'openai',baseUrl:'https://api.deepseek.com',selectedModel:'envpilot-deepseek-test-model',apiKey:'envpilot-not-a-real-deepseek-key'}})"
    Invoke-CdpExpression -Port $CdpPort -Expression "window.__TAURI_INTERNALS__.invoke('activate_ai_provider',{providerId:'openai'})"
    $AiTrayStatusJson = & node $CdpHelper $CdpPort "window.__TAURI_INTERNALS__.invoke('tray_menu_status')"
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to read the AI provider tray hierarchy."
    }
    $AiTrayStatus = $AiTrayStatusJson | ConvertFrom-Json
    if (
        $AiTrayStatus.aiProviderEntries -ne 2 -or
        $AiTrayStatus.activeAiProviderId -ne "openai"
    ) {
        throw "Tray AI provider hierarchy does not list the two valid independent configurations."
    }
    Invoke-CdpExpression -Port $CdpPort -Expression "window.__TAURI_INTERNALS__.invoke('activate_ai_provider',{providerId:'deepseek'})"
    $AiTrayStatusAfterSwitch = (& node $CdpHelper $CdpPort "window.__TAURI_INTERNALS__.invoke('tray_menu_status')") | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $AiTrayStatusAfterSwitch.activeAiProviderId -ne "deepseek") {
        throw "Tray-compatible AI provider switching did not persist the selected provider."
    }
    if (
        -not (Test-Path -LiteralPath (Join-Path $DataRoot "config\ai\providers\openai.json")) -or
        -not (Test-Path -LiteralPath (Join-Path $DataRoot "config\ai\providers\deepseek.json")) -or
        -not (Test-Path -LiteralPath (Join-Path $DataRoot "config\ai\secrets\openai.dpapi.json")) -or
        -not (Test-Path -LiteralPath (Join-Path $DataRoot "config\ai\secrets\deepseek.dpapi.json"))
    ) {
        throw "AI provider configuration and DPAPI secret files were not stored independently."
    }
    Save-WindowCapture -Process $FirstProcess -Path $SettingsScreenshot
    Invoke-CdpExpression -Port $CdpPort -Expression "window.__TAURI_INTERNALS__.invoke('plugin:event|emit',{event:'tray-action',payload:{kind:'selectAiProvider',providerId:'deepseek'}})"
    Start-Sleep -Seconds 1
    $ActiveAiProviderRendered = & node $CdpHelper $CdpPort "Boolean(document.querySelector('[data-ai-provider=""deepseek""].current') && document.querySelector('[data-ai-provider-form=""deepseek""]') && document.querySelector('#activate-ai-provider')?.disabled)"
    if ($LASTEXITCODE -ne 0 -or $ActiveAiProviderRendered -ne "true") {
        throw "The frontend did not synchronize the AI provider selected from the tray."
    }
    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('.ai-settings-section')?.scrollIntoView({block:'start'}); true"
    Start-Sleep -Seconds 1
    Save-WindowCapture -Process $FirstProcess -Path $AiProviderScreenshot

    Invoke-CdpExpression -Port $CdpPort -Expression "document.documentElement.dataset.theme='game-hud';document.querySelector('[data-nav=""dashboard""]')?.click();document.querySelector('.content')?.scrollTo(0,0);true"
    Start-Sleep -Seconds 1
    $GameHudRendered = & node $CdpHelper $CdpPort "(()=>{const root=getComputedStyle(document.documentElement);const panel=document.querySelector('.panel');const icon=document.querySelector('.quick-icon');return root.getPropertyValue('--accent').trim()==='#ff6a1f'&&getComputedStyle(document.querySelector('.app-shell')).backgroundImage.includes('data:image/svg+xml')&&getComputedStyle(panel).clipPath.includes('polygon')&&getComputedStyle(icon).clipPath.includes('polygon')})()"
    if ($LASTEXITCODE -ne 0 -or $GameHudRendered -ne "true") {
        throw "The black-orange honeycomb and hexagonal game HUD theme did not render as expected."
    }
    Save-WindowCapture -Process $FirstProcess -Path $GameHudScreenshot

    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('[data-nav=""tools""]')?.click(); true"
    Start-Sleep -Milliseconds 600
    Invoke-CdpExpression -Port $CdpPort -Expression "document.querySelector('[data-open-tool=""java""]')?.click(); true"
    Start-Sleep -Milliseconds 600
    $StoredNavigationJson = & node $CdpHelper $CdpPort "JSON.parse(localStorage.getItem('envnexus-ai.navigation'))"
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to read the persisted last-page state."
    }
    $StoredNavigation = $StoredNavigationJson | ConvertFrom-Json
    if (
        $StoredNavigation.view -ne "tool-detail" -or
        $StoredNavigation.selectedToolId -ne "java"
    ) {
        throw "The last tool detail page was not persisted before restart."
    }
}
finally {
    if (-not $FirstProcess.HasExited) {
        Stop-Process -Id $FirstProcess.Id -Force
    }
}

$SnapshotHashBeforeRestart = (Get-FileHash -LiteralPath $Snapshot -Algorithm SHA256).Hash
$SnapshotTimeBeforeRestart = (Get-Item -LiteralPath $Snapshot).LastWriteTimeUtc
$SnapshotData = Get-Content -LiteralPath $Snapshot -Raw -Encoding UTF8 | ConvertFrom-Json

$CdpPort += 1
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$CdpPort"
$SecondProcess = Start-Process -FilePath $Executable -PassThru
try {
    Wait-ForMainWindow -Process $SecondProcess
    Start-Sleep -Seconds $StartupWaitSeconds
    Wait-ForCdp -Port $CdpPort
    $RestoredLastView = & node $CdpHelper $CdpPort "Boolean(document.querySelector('.tool-detail-page') && document.querySelector('.tool-detail-identity h1')?.textContent.includes('Java'))"
    if ($LASTEXITCODE -ne 0 -or $RestoredLastView -ne "true") {
        throw "EnvNexus AI did not restore the last Java tool detail page after restart."
    }
    Save-WindowCapture -Process $SecondProcess -Path $RestartScreenshot
}
finally {
    if (-not $SecondProcess.HasExited) {
        Stop-Process -Id $SecondProcess.Id -Force
    }
}

$SnapshotHashAfterRestart = (Get-FileHash -LiteralPath $Snapshot -Algorithm SHA256).Hash
$SnapshotTimeAfterRestart = (Get-Item -LiteralPath $Snapshot).LastWriteTimeUtc
if ($SnapshotHashAfterRestart -ne $SnapshotHashBeforeRestart) {
    throw "Restart changed the cached scan snapshot, indicating an unexpected startup scan."
}
if ($SnapshotTimeAfterRestart -ne $SnapshotTimeBeforeRestart) {
    throw "Restart rewrote the cached scan snapshot, indicating an unexpected startup scan."
}

[pscustomobject]@{
    Executable = $Executable
    DataRoot = $DataRoot
    Snapshot = $Snapshot
    ScanFinishedAt = $SnapshotData.scanFinishedAt
    ToolCount = @($SnapshotData.tools).Count
    VersionManagerCount = @($SnapshotData.versionManagers).Count
    FreshStartupDidNotScan = $true
    ManualScanPersistedSnapshot = $true
    TrayToolEntryCount = $TrayMenuStatus.toolEntries
    TraySwitchEntryCount = $TrayMenuStatus.switchEntries
    TrayDiagnosticEntryCount = $TrayMenuStatus.diagnosticEntries
    TrayDiagnosticRepairEntryCount = $TrayMenuStatus.diagnosticRepairEntries
    TrayDiagnosticActionsVerified = $true
    RestartReusedSnapshot = $true
    UnscannedToolCount = $UnscannedToolCount
    CoreToolCount = $CoreToolCount
    AndroidToolCount = $AndroidToolCount
    StandaloneAndroidNavigation = $StandaloneAndroidNavigation
    TypedPythonRootPersisted = $true
    SnapshotSha256 = $SnapshotHashAfterRestart
    BeforeScanScreenshot = $BeforeScanScreenshot
    UnscannedToolsScreenshot = $UnscannedToolsScreenshot
    UnscannedPythonScreenshot = $UnscannedPythonScreenshot
    AfterScanScreenshot = $AfterScanScreenshot
    DiagnosticsScreenshot = $DiagnosticsScreenshot
    LocalGuidanceScreenshot = $LocalGuidanceScreenshot
    GuidanceCommandCount = $GuidanceCommandCount
    RepairPlanScreenshot = $RepairPlanScreenshot
    CommandsScreenshot = $CommandsScreenshot
    CommandToolCount = $CommandToolCount
    GeneratedCommandCount = $GeneratedCommandCount
    AiBrandIconCount = $AiBrandIconCount
    ModernRoundAiIcon = $ModernRoundAiIcon
    OtherThemesKeepOwnIconShape = $OtherThemeKeepsOwnIconShape
    SettingsButtonLayoutAligned = $true
    SettingsScrollPreserved = $true
    TrayAiProviderEntryCount = $AiTrayStatusAfterSwitch.aiProviderEntries
    ActiveTrayAiProvider = $AiTrayStatusAfterSwitch.activeAiProviderId
    IndependentAiProviderFiles = $true
    SettingsScreenshot = $SettingsScreenshot
    AiProviderScreenshot = $AiProviderScreenshot
    GameHudRendered = $true
    GameHudScreenshot = $GameHudScreenshot
    RestoredLastView = $true
    RestartScreenshot = $RestartScreenshot
} | Format-List
