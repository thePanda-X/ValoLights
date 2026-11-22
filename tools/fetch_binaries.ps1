# Download the third-party binaries and models the app needs (dev + build time).
#   powershell -ExecutionPolicy Bypass -File tools\fetch_binaries.ps1 [-Force]
#
# - bin\ffmpeg.exe, bin\ffprobe.exe   gyan.dev "essentials" build 9.0.2 (NVENC/NVDEC, libass, xfade, zscale)
# - bin\libmpv-2.dll                  shinchiro mpv-winbuild-cmake "mpv-dev-x86_64" 20261111
# - bin\rife\                         rife-ncnn-vulkan (MIT): RIFE v4.6 model (~17 MB kept) from the release, the exe
#                                     built from source with tools\rife\jobs.patch, OpenMP runtime
#                                     (needs git, CMake and the Visual Studio C++ build tools)
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
# The model and license come from the upstream release. The exe is built from the same tag plus
# tools\rife\jobs.patch (-T: interpolate only the listed time steps, so super smooth skips the in-between frames
# its shutter never uses). Building needs git, CMake and Visual Studio C++ build tools; the Vulkan headers and
# an import library for vulkan-1.dll (made from the loader's .def) are downloaded, so no Vulkan SDK is needed.
$Rife = Join-Path $Bin "rife"
$RifeCommit = "a7532fc3f9f8f008cd6eecd6f2ffe2a9698e0cf7"   # tag 20221029
$RifePatch = Join-Path $PSScriptRoot "rife\jobs.patch"
$VkHeadersUrl = "https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/v1.3.236.zip"
$VkHeadersSha = "7bf8d1f49b9a6fd0090cda7d30c446b9240643284f42a8254d27964462b158a6"
$VkDefUrl = "https://raw.githubusercontent.com/KhronosGroup/Vulkan-Loader/v1.3.236/loader/vulkan-1.def"
$VkDefSha = "166014c0b24e6d6b728a05fed1ea4b6a123ed2ac5d98f63f3a176da4e93ed75d"
$RifeStamp = "rife-ncnn-vulkan $RifeCommit + jobs.patch $((Get-FileHash $RifePatch -Algorithm SHA256).Hash.ToLower())"
$RifeStampFile = Join-Path $Rife "BUILD.txt"
$rifeOk = (Test-Path (Join-Path $Rife "rife-v4.6\flownet.bin")) -and (Test-Path $RifeStampFile) -and
    ((Get-Content $RifeStampFile -Raw).Trim() -eq $RifeStamp)
if ($Force -or -not $rifeOk) {
    Write-Host "== rife-ncnn-vulkan (model from the release, exe built from source)"
    $zip = Join-Path $Dl "rife.zip"
    Get-File $RifeUrl $zip $RifeSha
    $x = Join-Path $Dl "rife"
    if (Test-Path $x) { Remove-Item -Recurse -Force $x }
    New-Item -ItemType Directory $x | Out-Null
    & $Tar -xf $zip -C $x
    $rel = Join-Path $x "rife-ncnn-vulkan-20221029-windows"

    # sources: the tag, its submodules (ncnn, glslang, libwebp), then the patch
    $Tools = Join-Path $Root "build\tools"
    $RSrc = Join-Path $Tools "rife-src\src"
    if (-not (Test-Path (Join-Path $RSrc ".git"))) {
        if (Test-Path (Join-Path $Tools "rife-src")) { Remove-Item -Recurse -Force (Join-Path $Tools "rife-src") }
        & git -c advice.detachedHead=false clone -q --depth 1 --branch 20221029 https://github.com/nihui/rife-ncnn-vulkan.git $RSrc
        if ($LASTEXITCODE -ne 0) { throw "rife: clone failed" }
    }
    if ((& git -C $RSrc rev-parse HEAD) -ne $RifeCommit) { throw "rife: unexpected source commit" }
    & git -C $RSrc submodule update --init --recursive --depth 1 -q
    if ($LASTEXITCODE -ne 0) { throw "rife: submodule update failed" }
    & git -C $RSrc checkout -q -- .
    & git -C $RSrc apply (Resolve-Path $RifePatch)
    if ($LASTEXITCODE -ne 0) { throw "rife: jobs.patch does not apply" }

    # Vulkan headers + vulkan-1.lib
    $Vk = Join-Path $Tools "rife-src\vk"
    if (Test-Path $Vk) { Remove-Item -Recurse -Force $Vk }
    New-Item -ItemType Directory -Force $Vk | Out-Null
    $vkZip = Join-Path $Dl "vulkan-headers.zip"
    Get-File $VkHeadersUrl $vkZip $VkHeadersSha
    & $Tar -xf $vkZip -C $Vk
    $vkDef = Join-Path $Vk "vulkan-1.def"
    Get-File $VkDefUrl $vkDef $VkDefSha
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    $libExe = & $vswhere -latest -products * -find "VC\Tools\MSVC\**\bin\Hostx64\x64\lib.exe" | Select-Object -First 1
    if (-not $libExe) { throw "rife: Visual Studio C++ build tools not found" }
    $vkLib = Join-Path $Vk "vulkan-1.lib"
    & $libExe -nologo "-def:$vkDef" "-out:$vkLib" -machine:x64 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "rife: making vulkan-1.lib failed" }

    $RBuild = Join-Path $Tools "rife-src\build"
    if (Test-Path $RBuild) { Remove-Item -Recurse -Force $RBuild }
    & cmake -S (Join-Path $RSrc "src") -B $RBuild -A x64 -Wno-dev -Wno-deprecated "-DCMAKE_POLICY_VERSION_MINIMUM=3.5" `
        "-DVulkan_INCLUDE_DIR=$(Join-Path $Vk 'Vulkan-Headers-1.3.236\include')" "-DVulkan_LIBRARY=$vkLib" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "rife: cmake configure failed" }
    & cmake --build $RBuild --config Release -j | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "rife: build failed" }
    # the OpenMP runtime of the toolset that built it
    $vcomp = & $vswhere -latest -products * -find "VC\Redist\MSVC\**\x64\Microsoft.*.OpenMP\vcomp140.dll" |
        Where-Object { $_ -notmatch "\\(onecore|spectre)\\" } | Select-Object -First 1
    if (-not $vcomp) { throw "rife: vcomp140.dll not found in the Visual Studio redist" }

    if (Test-Path $Rife) { Remove-Item -Recurse -Force $Rife }
    New-Item -ItemType Directory -Force (Join-Path $Rife "rife-v4.6") | Out-Null
    Copy-Item (Join-Path $RBuild "Release\rife-ncnn-vulkan.exe"), $vcomp $Rife -Force
    Copy-Item (Join-Path $rel "LICENSE") (Join-Path $Rife "LICENSE-rife-ncnn-vulkan.txt") -Force
    Copy-Item (Join-Path $rel "rife-v4.6\flownet.bin"), (Join-Path $rel "rife-v4.6\flownet.param") (Join-Path $Rife "rife-v4.6") -Force
    Set-Content -Path $RifeStampFile -Value $RifeStamp -Encoding ascii
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
