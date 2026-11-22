# CI only: load the MSVC developer environment of the newest toolset that really has cl.exe into the following
# steps (GITHUB_ENV).
#
# Why: vl-speech builds whisper.cpp with CMake through the `cmake` crate, which hands CMake the compiler
# environment the `cc` crate found. On a runner image with several MSVC versions that can be an older one than
# the toolset CMake's Visual Studio generator uses; on image win25-vs2026 20261116 it was 14.51, whose cl.exe
# is gone, so ggml's ASM check failed ("not a full path to an existing compiler tool") while C/C++ used 14.52.
# Inside a developer environment (VCINSTALLDIR set) `cc` uses the tools on PATH, so everything agrees.
$ErrorActionPreference = "Stop"
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw "no Visual Studio with the C++ tools found" }
$all = Get-ChildItem (Join-Path $vs "VC\Tools\MSVC") -Directory
$ok = $all | Where-Object { Test-Path (Join-Path $_.FullName "bin\Hostx64\x64\cl.exe") } | Sort-Object { [version]$_.Name } -Descending
$default = Get-Content (Join-Path $vs "VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt") -ErrorAction SilentlyContinue
Write-Host "Visual Studio: $vs"
Write-Host "MSVC toolsets: $($all.Name -join ', ') (with cl.exe: $($ok.Name -join ', '); default: $default)"
if (-not $ok) { throw "no MSVC toolset with cl.exe" }
$ver = $ok[0].Name
$vcvars = Join-Path $vs "VC\Auxiliary\Build\vcvars64.bat"
$lines = cmd /c "`"$vcvars`" -vcvars_ver=$ver >nul 2>&1 && set"
if ($LASTEXITCODE -ne 0) { throw "vcvars64.bat -vcvars_ver=$ver failed" }
# only what vcvars added or changed (and never the runner's own variables)
$n = 0
foreach ($line in $lines) {
    if ($line -notmatch '^([^=]+)=(.*)$') { continue }
    $name, $value = $matches[1], $matches[2]
    if ($name -match '^(GITHUB_|RUNNER_|ACTIONS_)') { continue }
    if ([Environment]::GetEnvironmentVariable($name) -eq $value) { continue }
    "$name=$value" | Out-File -Append -Encoding utf8 $env:GITHUB_ENV
    $n++
}
Write-Host "MSVC $ver developer environment: $n variables set"
