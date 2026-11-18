# Dev check: run the installer, save each wizard page of its own window (PrintWindow, not the screen) and
# cancel without installing.  powershell -File tools\installer_shots.ps1 <setup.exe> <out_dir>
param($Setup, $Out)
$ErrorActionPreference = "Stop"
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @"
using System; using System.Text; using System.Drawing; using System.Runtime.InteropServices; using System.Collections.Generic;
public static class W {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr h, EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  public struct RECT { public int L, T, R, B; }
  public static string Text(IntPtr h) { var s = new StringBuilder(256); GetWindowText(h, s, 256); return s.ToString(); }
  public static IntPtr Child(IntPtr p, string text) {
    IntPtr found = IntPtr.Zero;
    EnumChildWindows(p, (h, l) => { if (IsWindowVisible(h) && Text(h).Replace("&", "") == text) { found = h; return false; } return true; }, IntPtr.Zero);
    return found;
  }
  public static IntPtr Top(string title) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => { if (IsWindowVisible(h) && Text(h).StartsWith(title)) { found = h; return false; } return true; }, IntPtr.Zero);
    return found;
  }
  public static void Save(IntPtr h, string path) {
    RECT r; GetWindowRect(h, out r);
    using (var b = new Bitmap(r.R - r.L, r.B - r.T)) {
      using (var g = Graphics.FromImage(b)) { var dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); }
      b.Save(path, System.Drawing.Imaging.ImageFormat.Png);
    }
  }
  public static void Click(IntPtr h) { SendMessage(h, 0x00F5, IntPtr.Zero, IntPtr.Zero); }
}
"@
New-Item -ItemType Directory -Force $Out | Out-Null
$p = Start-Process $Setup -ArgumentList "/DIR=`"$env:TEMP\vl_setup_shot_never_installed`"" -PassThru
$w = [IntPtr]::Zero
for ($i = 0; $i -lt 60 -and $w -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 500; $w = [W]::Top("Setup - ValoLights") }
if ($w -eq [IntPtr]::Zero) { throw "setup window not found" }
$n = 1
foreach ($page in "welcome", "folder", "tasks", "ready") {
    Start-Sleep -Milliseconds 1200
    [W]::Save($w, (Join-Path $Out ("{0}_{1}.png" -f $n, $page)))
    $n++
    if ($page -ne "ready") { [W]::Click([W]::Child($w, "Next")) }
}
# cancel, capture the confirmation, confirm
[W]::Click([W]::Child($w, "Cancel"))
Start-Sleep -Milliseconds 1200
$d = [W]::Top("Exit Setup")
if ($d -ne [IntPtr]::Zero) {
    [W]::Save($d, (Join-Path $Out "5_exit.png"))
    $yes = [W]::Child($d, "Yes")
    if ($yes -ne [IntPtr]::Zero) { [W]::Click($yes) } else { Stop-Process -Id $p.Id -Force }
}
Start-Sleep -Seconds 2
Get-Process | Where-Object { $_.Path -like "*ValoLights-Setup*" -or $_.ProcessName -like "ValoLights-Setup*" } | Stop-Process -Force -ErrorAction SilentlyContinue
"installed: $(Test-Path "$env:TEMP\vl_setup_shot_never_installed")"



