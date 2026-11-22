# CI only: forget CMake configurations, restored with the Rust build cache, that point at tools the runner no
# longer has.
#
# Why: vl-speech builds whisper.cpp with CMake in target\...\whisper-rs-sys-*\out\build, and that folder comes
# back from the build cache. CMake keeps the compiler paths it found in CMakeCache.txt; after the runner image
# replaced MSVC 14.51 with 14.52 (win25-vs2026 20261116) the cached ASM compiler pointed at a cl.exe that was
# gone ("CMAKE_ASM_COMPILER ... is not a full path to an existing compiler tool"). Dropping the stale
# CMakeCache.txt makes CMake configure afresh; the rest of the cache is still used.
$ErrorActionPreference = "Stop"
$root = Join-Path (Split-Path -Parent $PSScriptRoot) "target"
if (-not (Test-Path $root)) { Write-Host "no target folder"; exit 0 }
$caches = Get-ChildItem $root -Directory | ForEach-Object { Join-Path $_.FullName "build" } | Where-Object { Test-Path $_ } |
    ForEach-Object { Get-ChildItem $_ -Recurse -Depth 4 -Filter CMakeCache.txt -File -ErrorAction SilentlyContinue }
$dropped = 0
foreach ($c in $caches) {
    $missing = @()
    foreach ($line in Get-Content $c.FullName) {
        # e.g. CMAKE_ASM_COMPILER:FILEPATH=C:/Program Files/.../cl.exe
        if ($line -match '^(CMAKE_\w*(COMPILER|LINKER|_AR|_MT|_RC_COMPILER))(:\w+)?=(.+)$') {
            $name, $path = $matches[1], $matches[4].Trim()
            if ($path -match '^[A-Za-z]:[\\/]' -and -not (Test-Path -LiteralPath $path)) { $missing += "$name = $path" }
        }
    }
    if ($missing) {
        Write-Host "stale CMake cache $($c.FullName):"
        $missing | ForEach-Object { Write-Host "  missing $_" }
        Remove-Item -LiteralPath $c.FullName -Force
        $files = Join-Path $c.DirectoryName "CMakeFiles"
        if (Test-Path $files) { Remove-Item -LiteralPath $files -Recurse -Force }
        $dropped++
    }
}
Write-Host "$(@($caches).Count) CMake caches checked, $dropped dropped"
