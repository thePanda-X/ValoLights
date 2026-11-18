# Build the ValoLights installer: dist\ValoLights-Setup-<version>.exe
#   powershell -ExecutionPolicy Bypass -File build\installer.ps1 [-SkipBuild]
#
# 1. build\build.ps1              builds, assembles build\out\ValoLights and self-tests it
# 2. stages that folder without the parts the setup downloads itself (ffmpeg, ffprobe, libmpv, cuBLAS)
# 3. compiles build\installer.iss with Inno Setup 6 (winget install JRSoftware.InnoSetup)
param([switch]$SkipBuild)
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$Version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if (-not $SkipBuild) {
    & powershell -ExecutionPolicy Bypass -File (Join-Path $Root "build\build.ps1")
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
}
$App = Join-Path $Root "build\out\ValoLights"
if (-not (Test-Path (Join-Path $App "ValoLights.exe"))) { throw "no assembled app in $App (run without -SkipBuild)" }

Write-Host "== Staging the installer payload"
$Payload = Join-Path $Root "build\out\installer-payload"
if (Test-Path $Payload) { Remove-Item -Recurse -Force $Payload }
Copy-Item $App $Payload -Recurse
foreach ($f in "bin\ffmpeg.exe", "bin\ffprobe.exe", "bin\libmpv-2.dll", "bin\FFMPEG_LICENSE.txt", "cublas64_13.dll", "cublasLt64_13.dll") {
    $p = Join-Path $Payload $f
    if (Test-Path $p) { Remove-Item -Force $p }
}

$Iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $Iscc) { throw "Inno Setup 6 not found: winget install JRSoftware.InnoSetup" }
Write-Host "== Compiling the installer"
& $Iscc /Q "/DAppVersion=$Version" "/DPayload=$Payload" (Join-Path $Root "build\installer.iss")
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
$Exe = Join-Path $Root "dist\ValoLights-Setup-$Version.exe"
Write-Host ("== Done: {0} ({1} MB)" -f $Exe, [math]::Round((Get-Item $Exe).Length / 1MB))
