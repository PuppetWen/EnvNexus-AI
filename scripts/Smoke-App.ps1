param(
    [int]$WaitSeconds = 40
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$Executable = Join-Path $ProjectRoot "src-tauri\target\release\envnexus-ai.exe"
$ArtifactRoot = Join-Path $ProjectRoot "artifacts\smoke"
$DataRoot = Join-Path $ArtifactRoot "data"
$Screenshot = Join-Path $ArtifactRoot "envnexus-ai-runtime.png"

if (-not (Test-Path -LiteralPath $Executable)) {
    throw "Release executable not found. Run scripts\Invoke-Rust.ps1 -Task tauri first."
}
New-Item -ItemType Directory -Path $ArtifactRoot -Force | Out-Null
$env:ENVNEXUS_AI_DATA_ROOT = $DataRoot

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EnvNexusAIWindowCapture {
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int command);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint flags);
}
'@

$Process = Start-Process -FilePath $Executable -PassThru
try {
    Start-Sleep -Seconds $WaitSeconds
    $Process.Refresh()
    if ($Process.HasExited) {
        throw "EnvNexus AI exited during startup with code $($Process.ExitCode)."
    }
    if ($Process.MainWindowHandle -eq 0) {
        throw "EnvNexus AI did not create a main window."
    }

    [EnvNexusAIWindowCapture]::ShowWindow($Process.MainWindowHandle, 3) | Out-Null
    [EnvNexusAIWindowCapture]::SetForegroundWindow($Process.MainWindowHandle) | Out-Null
    Start-Sleep -Seconds 2

    [EnvNexusAIWindowCapture]::SetProcessDPIAware() | Out-Null
    $Rect = New-Object EnvNexusAIWindowCapture+RECT
    if (-not [EnvNexusAIWindowCapture]::GetWindowRect($Process.MainWindowHandle, [ref]$Rect)) {
        throw "EnvNexus AI window bounds could not be read."
    }
    $Width = $Rect.Right - $Rect.Left
    $Height = $Rect.Bottom - $Rect.Top
    if ($Width -le 0 -or $Height -le 0) {
        throw "EnvNexus AI returned invalid window bounds."
    }

    Add-Type -AssemblyName System.Drawing
    $Bitmap = New-Object System.Drawing.Bitmap $Width, $Height
    $Graphics = [System.Drawing.Graphics]::FromImage($Bitmap)
    try {
        $Hdc = $Graphics.GetHdc()
        try {
            if (-not [EnvNexusAIWindowCapture]::PrintWindow($Process.MainWindowHandle, $Hdc, 2)) {
                throw "EnvNexus AI window capture failed."
            }
        }
        finally {
            $Graphics.ReleaseHdc($Hdc)
        }
        $Bitmap.Save($Screenshot, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $Graphics.Dispose()
        $Bitmap.Dispose()
    }

    [pscustomobject]@{
        Executable = $Executable
        ProcessId = $Process.Id
        WindowTitle = $Process.MainWindowTitle
        DataRoot = $DataRoot
        Screenshot = $Screenshot
        Width = $Width
        Height = $Height
    } | Format-List
}
finally {
    if (-not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force
    }
}
