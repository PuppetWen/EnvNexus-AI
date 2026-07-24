param(
    [int]$WaitSeconds = 25
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$Executable = Join-Path $ProjectRoot "src-tauri\target\release\envnexus-ai.exe"
$ArtifactRoot = Join-Path $ProjectRoot "artifacts\smoke"
$DataRoot = Join-Path $ArtifactRoot "data"
$DetailBefore = Join-Path $ArtifactRoot "envnexus-ai-tool-detail.png"
$DetailAfter = Join-Path $ArtifactRoot "envnexus-ai-tool-detail-scrolled.png"

if (-not (Test-Path -LiteralPath $Executable)) {
    throw "Release executable not found. Build the release executable first."
}

New-Item -ItemType Directory -Path $ArtifactRoot -Force | Out-Null
$env:ENVNEXUS_AI_DATA_ROOT = $DataRoot

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EnvNexusAIToolSmoke {
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int command);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint flags);

  public static void LeftClick() {
    mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);
    mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
  }

  public static void Wheel(int delta) {
    mouse_event(0x0800, 0, 0, unchecked((uint)delta), UIntPtr.Zero);
  }
}
'@

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

function Save-WindowCapture {
    param(
        [IntPtr]$WindowHandle,
        [EnvNexusAIToolSmoke+RECT]$Bounds,
        [string]$Path
    )

    $Width = $Bounds.Right - $Bounds.Left
    $Height = $Bounds.Bottom - $Bounds.Top
    $Bitmap = New-Object System.Drawing.Bitmap $Width, $Height
    $Graphics = [System.Drawing.Graphics]::FromImage($Bitmap)
    try {
        $Hdc = $Graphics.GetHdc()
        try {
            if (-not [EnvNexusAIToolSmoke]::PrintWindow($WindowHandle, $Hdc, 2)) {
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

$Process = Start-Process -FilePath $Executable -PassThru
try {
    Start-Sleep -Seconds $WaitSeconds
    $Process.Refresh()
    if ($Process.HasExited -or $Process.MainWindowHandle -eq 0) {
        throw "EnvNexus AI did not keep a usable main window."
    }

    [EnvNexusAIToolSmoke]::SetProcessDPIAware() | Out-Null
    [EnvNexusAIToolSmoke]::ShowWindow($Process.MainWindowHandle, 3) | Out-Null
    [EnvNexusAIToolSmoke]::SetForegroundWindow($Process.MainWindowHandle) | Out-Null
    Start-Sleep -Seconds 2

    $Bounds = New-Object EnvNexusAIToolSmoke+RECT
    if (-not [EnvNexusAIToolSmoke]::GetWindowRect($Process.MainWindowHandle, [ref]$Bounds)) {
        throw "EnvNexus AI window bounds could not be read."
    }

    # Open the tool library, then the first tool's independent management page.
    [EnvNexusAIToolSmoke]::SetCursorPos($Bounds.Left + 145, $Bounds.Top + 290) | Out-Null
    [EnvNexusAIToolSmoke]::LeftClick()
    Start-Sleep -Seconds 2
    [EnvNexusAIToolSmoke]::SetCursorPos($Bounds.Left + 625, $Bounds.Top + 720) | Out-Null
    [EnvNexusAIToolSmoke]::LeftClick()
    Start-Sleep -Seconds 2

    # Keep the pointer in the scrollable content area for both captures.
    [EnvNexusAIToolSmoke]::SetCursorPos($Bounds.Left + 1500, $Bounds.Top + 800) | Out-Null
    Save-WindowCapture -WindowHandle $Process.MainWindowHandle -Bounds $Bounds -Path $DetailBefore
    1..8 | ForEach-Object { [EnvNexusAIToolSmoke]::Wheel(-120) }
    Start-Sleep -Seconds 2
    Save-WindowCapture -WindowHandle $Process.MainWindowHandle -Bounds $Bounds -Path $DetailAfter

    $BeforeHash = (Get-FileHash -LiteralPath $DetailBefore -Algorithm SHA256).Hash
    $AfterHash = (Get-FileHash -LiteralPath $DetailAfter -Algorithm SHA256).Hash
    if ($BeforeHash -eq $AfterHash) {
        throw "Mouse-wheel smoke test did not change the tool detail viewport."
    }

    [pscustomobject]@{
        Executable = $Executable
        ToolDetail = $DetailBefore
        ScrolledToolDetail = $DetailAfter
        BeforeSha256 = $BeforeHash
        AfterSha256 = $AfterHash
        WheelViewportChanged = $true
    } | Format-List
}
finally {
    if (-not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force
    }
}
