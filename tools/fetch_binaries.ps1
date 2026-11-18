# Download the third-party binaries and models the app needs (dev + build time).
#   powershell -ExecutionPolicy Bypass -File tools\fetch_binaries.ps1 [-Force]
#
# - bin\ffmpeg.exe, bin\ffprobe.exe   gyan.dev "essentials" build 9.0.2 (NVENC/NVDEC, libass, xfade, zscale)
# - bin\libmpv-2.dll                  shinchiro mpv-winbuild-cmake "mpv-dev-x86_64" 20261111
# - bin\rife\                         rife-ncnn-vulkan (MIT): exe, OpenMP runtime, RIFE v4.6 model (~17 MB kept)
# - assets\ocr\PP-OCRv6_*.onnx        RapidOCR's PP-OCRv6 small models (checked against their SHA-256)
# - assets\audio\yamnet.onnx         YAMNet sound events (Apache 2.0) for Smart cut, checked against its SHA-256
# - build\tools\libclang\             libclang.dll (build only: whisper-rs generates its C bindings with it), taken
#                                     from the standalone "libclang" wheel on PyPI (a zip; no Python involved)
# The speech model is downloaded by the app itself on first use (data\models\whisper-ggml).
param([switch]$Force)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # Invoke-WebRequest is very slow with the progress bar
$Root = Split-Path -Parent $PSScriptRoot
$Bin = Join-Path $Root "bin"
$Dl = Join-Path $Root "build\downloads"
$Tar = Join-Path $env:SystemRoot "System32\tar.exe"   # libarchive: zip and 7z (incl. BCJ2)
New-Item -ItemType Directory -Force $Bin, $Dl | Out-Null

function Get-File($url, $out, $sha = "") {
    Write-Host "  downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $out -UserAgent "valolights-build"
    if ($sha -ne "" -and (Get-FileHash $out -Algorithm SHA256).Hash.ToLower() -ne $sha) { throw "$(Split-Path -Leaf $out): checksum mismatch" }
}

# Pinned third-party releases. The installer (build\installer.iss) downloads the same files at install time:
# keep the URLs and checksums of both in step when updating one of them.
$FfmpegUrl = "https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip"
$FfmpegSha = "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba"
$MpvUrl = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261111/mpv-dev-x86_64-20261111-git-bcde6b93cc.7z"
$MpvSha = "273c079b11ea5575822f972756f4c9a71149a5c10130353abeb8586738938452"
$RifeUrl = "https://github.com/nihui/rife-ncnn-vulkan/releases/download/20221029/rife-ncnn-vulkan-20221029-windows.zip"
$RifeSha = "d8e4d772d26cd8006ef0ad0bc82eb191b53c68677d1ae2f42506d74cbbbea606"

# ---------------------------------------------------------------- ffmpeg
if ($Force -or -not (Test-Path (Join-Path $Bin "ffmpeg.exe"))) {
    Write-Host "== ffmpeg"
    $zip = Join-Path $Dl "ffmpeg.zip"
    Get-File $FfmpegUrl $zip $FfmpegSha
    $x = Join-Path $Dl "ffmpeg"
    if (Test-Path $x) { Remove-Item -Recurse -Force $x }
    New-Item -ItemType Directory $x | Out-Null
    & $Tar -xf $zip -C $x
    $top = Get-ChildItem $x -Directory | Select-Object -First 1
    Copy-Item (Join-Path $top.FullName "bin\ffmpeg.exe"), (Join-Path $top.FullName "bin\ffprobe.exe") $Bin -Force
    Copy-Item (Join-Path $top.FullName "LICENSE") (Join-Path $Bin "FFMPEG_LICENSE.txt") -Force
} else { Write-Host "ffmpeg present" }

# ---------------------------------------------------------------- libmpv
if ($Force -or -not (Test-Path (Join-Path $Bin "libmpv-2.dll"))) {
    Write-Host "== libmpv"
    $arc = Join-Path $Dl "mpv.7z"
    Get-File $MpvUrl $arc $MpvSha
    $x = Join-Path $Dl "mpv"
    if (Test-Path $x) { Remove-Item -Recurse -Force $x }
    New-Item -ItemType Directory $x | Out-Null
    & $Tar -xf $arc -C $x
    Copy-Item (Get-ChildItem $x -Recurse -Filter "libmpv-2.dll" | Select-Object -First 1).FullName $Bin -Force
} else { Write-Host "libmpv present" }

# ---------------------------------------------------------------- RIFE
$Rife = Join-Path $Bin "rife"
if ($Force -or -not (Test-Path (Join-Path $Rife "rife-v4.6\flownet.bin"))) {
    Write-Host "== rife-ncnn-vulkan"
    $zip = Join-Path $Dl "rife.zip"
    Get-File $RifeUrl $zip $RifeSha
    $x = Join-Path $Dl "rife"
    if (Test-Path $x) { Remove-Item -Recurse -Force $x }
    New-Item -ItemType Directory $x | Out-Null
    & $Tar -xf $zip -C $x
    $src = Join-Path $x "rife-ncnn-vulkan-20221029-windows"
    New-Item -ItemType Directory -Force (Join-Path $Rife "rife-v4.6") | Out-Null
    Copy-Item (Join-Path $src "rife-ncnn-vulkan.exe"), (Join-Path $src "vcomp140.dll") $Rife -Force
    Copy-Item (Join-Path $src "LICENSE") (Join-Path $Rife "LICENSE-rife-ncnn-vulkan.txt") -Force
    Copy-Item (Join-Path $src "rife-v4.6\flownet.bin"), (Join-Path $src "rife-v4.6\flownet.param") (Join-Path $Rife "rife-v4.6") -Force
} else { Write-Host "rife present" }

# ---------------------------------------------------------------- OCR models
$Ocr = Join-Path $Root "assets\ocr"
New-Item -ItemType Directory -Force $Ocr | Out-Null
$models = @(
    @{ name = "PP-OCRv6_rec_small.onnx"; url = "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.10.0/onnx/PP-OCRv6/rec/PP-OCRv6_rec_small.onnx"; sha = "6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884" },
    @{ name = "PP-OCRv6_det_small.onnx"; url = "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.10.0/onnx/PP-OCRv6/det/PP-OCRv6_det_small.onnx"; sha = "090f04abcd9d9a7498bc4ebf677e4cb9bdce1fe4197ddb7e529f1ef44e1ff94f" }
)
foreach ($m in $models) {
    $p = Join-Path $Ocr $m.name
    $ok = (Test-Path $p) -and ((Get-FileHash $p -Algorithm SHA256).Hash.ToLower() -eq $m.sha)
    if ($Force -or -not $ok) {
        Write-Host "== OCR model $($m.name)"
        Get-File $m.url $p
        if ((Get-FileHash $p -Algorithm SHA256).Hash.ToLower() -ne $m.sha) { throw "$($m.name): checksum mismatch" }
    } else { Write-Host "$($m.name) present" }
}
# ---------------------------------------------------------------- sound model (Smart cut)
# YAMNet (Google, Apache 2.0), ONNX export with the log-mel frontend inside: audiomagic/yamnet-onnx
$Aud = Join-Path $Root "assets\audio"
New-Item -ItemType Directory -Force $Aud | Out-Null
$yam = @(
    @{ name = "yamnet.onnx"; url = "https://huggingface.co/audiomagic/yamnet-onnx/resolve/main/yamnet.onnx"; sha = "d3835ffbbd4a1bb3e777f0ca217b5007907f5171dd5d17c4236b95b2af8f908e" },
    @{ name = "LICENSE-yamnet.txt"; url = "https://huggingface.co/audiomagic/yamnet-onnx/resolve/main/LICENSE"; sha = "" }
)
foreach ($m in $yam) {
    $p = Join-Path $Aud $m.name
    $ok = (Test-Path $p) -and ($m.sha -eq "" -or (Get-FileHash $p -Algorithm SHA256).Hash.ToLower() -eq $m.sha)
    if ($Force -or -not $ok) {
        Write-Host "== sound model $($m.name)"
        Get-File $m.url $p
        if ($m.sha -ne "" -and (Get-FileHash $p -Algorithm SHA256).Hash.ToLower() -ne $m.sha) { throw "$($m.name): checksum mismatch" }
    } else { Write-Host "$($m.name) present" }
}
# ---------------------------------------------------------------- libclang (build tool)
$Lc = Join-Path $Root "build\tools\libclang"
if ($Force -or -not (Test-Path (Join-Path $Lc "libclang-18.1.1.data\platlib\clang\native\libclang.dll"))) {
    Write-Host "== libclang (build tool)"
    New-Item -ItemType Directory -Force $Lc | Out-Null
    $whl = Join-Path $Lc "libclang.zip"
    Get-File "https://files.pythonhosted.org/packages/0b/2d/3f480b1e1d31eb3d6de5e3ef641954e5c67430d5ac93b7fa7e07589576c7/libclang-18.1.1-py2.py3-none-win_amd64.whl" $whl
    if ((Get-FileHash $whl -Algorithm SHA256).Hash.ToLower() -ne "4dd2d3b82fab35e2bf9ca717d7b63ac990a3519c7e312f19fa8e86dcc712f7fb") { throw "libclang: checksum mismatch" }
    & $Tar -xf $whl -C $Lc
} else { Write-Host "libclang present" }
Write-Host "== all binaries present"
