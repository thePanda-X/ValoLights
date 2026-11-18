# Build ValoLights and assemble the app folder the installer packages (build\installer.ps1 runs this first).
#   powershell -ExecutionPolicy Bypass -File build\build.ps1
# Output: build\out\ValoLights, self-tested
#
# Needs: Rust (stable, MSVC), CMake, the CUDA toolkit (13.x, for vl-speech's whisper.cpp kernels).
# Everything else is downloaded by tools\fetch_binaries.ps1.
#
# The app folder:
#   ValoLights.exe            the app (GUI, background worker, CLI, self-test)
#   vl-speech.exe             speech-to-text (whisper.cpp), started by the app for transcriptions
#   cudart/cublas DLLs        NVIDIA's CUDA runtime for vl-speech (the app itself never loads them)
#   bin\                      ffmpeg, ffprobe, libmpv-2.dll, rife\
#   assets\                   fonts, icons, templates, OCR models
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$Version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
Write-Host "== ValoLights $Version"

Write-Host "== Fetching ffmpeg, libmpv, RIFE, OCR models and libclang"
& powershell -ExecutionPolicy Bypass -File (Join-Path $Root "tools\fetch_binaries.ps1")
if ($LASTEXITCODE -ne 0) { throw "fetch_binaries failed" }

Write-Host "== cargo build --release (the first build compiles whisper.cpp's CUDA kernels: ~25 min)"
# cargo prints nothing while whisper.cpp's CUDA kernels compile, so report progress every minute
$cargo = Start-Process cargo -ArgumentList "build", "--release", "-p", "vl-app", "-p", "vl-speech" -NoNewWindow -PassThru
$null = $cargo.Handle   # keeps the exit code readable after the process ends
$started = Get-Date
while (-not $cargo.WaitForExit(60000)) {
    $line = "   ... still building ($([int]((Get-Date) - $started).TotalMinutes) min)"
    $proj = Get-ChildItem "target/release/build/whisper-rs-sys-*/out/build/ggml/src/ggml-cuda/ggml-cuda.vcxproj" -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($proj) {
        $total = (Select-String -Path $proj.FullName -Pattern "<CudaCompile Include=").Count
        $done = @(Get-ChildItem (Join-Path $proj.DirectoryName "ggml-cuda.dir/Release/*.obj") -ErrorAction SilentlyContinue).Count
        if ($total -gt 0 -and $done -lt $total) { $line += ", whisper.cpp CUDA kernels: $done of $total files" }
    }
    Write-Host $line
}
if ($cargo.ExitCode -ne 0) { throw "cargo build failed ($($cargo.ExitCode))" }

$Out = Join-Path $Root "build/out"
$App = Join-Path $Out "ValoLights"
if (Test-Path $App) { Remove-Item -Recurse -Force $App }
New-Item -ItemType Directory -Force $App | Out-Null

Write-Host "== Assembling"
Copy-Item target/release/valolights.exe (Join-Path $App "ValoLights.exe")
Copy-Item target/release/vl-speech.exe $App
# the CUDA toolkit the speech helper was built against (same as .cargo/config.toml)
$Cuda = "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.2\bin\x64"
foreach ($d in "cudart64_13.dll", "cublas64_13.dll", "cublasLt64_13.dll") { Copy-Item (Join-Path $Cuda $d) $App }
# the Visual C++ runtime both exes link against, next to them (app-local, as Microsoft allows): a PC without
# the VC++ redistributable could not start the app otherwise
$Crt = Get-ChildItem "C:\Program Files*\Microsoft Visual Studio\*\*\VC\Redist\MSVC\*\x64\Microsoft.VC14*.CRT" -Directory -ErrorAction SilentlyContinue |
    Sort-Object { [version]($_.Parent.Parent.Name) } | Select-Object -Last 1
if (-not $Crt) { throw "Visual C++ redistributable files not found (install the VS Build Tools)" }
foreach ($d in "msvcp140.dll", "msvcp140_1.dll", "vcruntime140.dll", "vcruntime140_1.dll") { Copy-Item (Join-Path $Crt.FullName $d) $App }
New-Item -ItemType Directory -Force (Join-Path $App "bin") | Out-Null
Copy-Item bin/ffmpeg.exe, bin/ffprobe.exe, bin/libmpv-2.dll, bin/FFMPEG_LICENSE.txt (Join-Path $App "bin") -Force
Copy-Item bin/rife (Join-Path $App "bin") -Recurse -Force
New-Item -ItemType Directory -Force (Join-Path $App "assets") | Out-Null
foreach ($d in "fonts", "icons", "templates", "ocr", "audio") { Copy-Item (Join-Path "assets" $d) (Join-Path $App "assets") -Recurse -Force }
Copy-Item assets/icon.ico, assets/icon.png (Join-Path $App "assets") -Force
Copy-Item build/README.txt (Join-Path $App "README.txt") -Force
New-Item -ItemType Directory -Force (Join-Path $App "licenses") | Out-Null
Copy-Item assets/fonts/OFL-*.txt (Join-Path $App "licenses") -Force
Copy-Item assets/icons/LICENSE-lucide.txt (Join-Path $App "licenses") -Force
Copy-Item LICENSE (Join-Path $App "licenses\ValoLights-LICENSE.txt") -Force
Copy-Item docs/third-party.md (Join-Path $App "licenses\THIRD-PARTY.md") -Force
Copy-Item assets/audio/LICENSE-yamnet.txt (Join-Path $App "licenses") -Force
Copy-Item bin/rife/LICENSE-rife-ncnn-vulkan.txt (Join-Path $App "licenses") -Force

Write-Host "== Self-test of the packaged app"
$p = Start-Process -FilePath (Join-Path $App "ValoLights.exe") -ArgumentList "--selftest" -Wait -PassThru -NoNewWindow
Get-Content (Join-Path $App "data/logs/selftest.txt")
if ($p.ExitCode -ne 0) { throw "self-test failed" }
Remove-Item -Recurse -Force (Join-Path $App "data")
Write-Host "== Done: $App"
