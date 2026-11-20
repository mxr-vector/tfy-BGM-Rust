# Build a portable Windows package: tfy-bgm.exe + an external bgm/ folder.
#
# Usage (from the repository root):
#   powershell -ExecutionPolicy Bypass -File scripts/package-portable.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/package-portable.ps1 -EmptyBgm
#
# Default: copy the repository bgm/ folder next to the exe (self-contained, ~350 MB).
# -EmptyBgm: create an empty category scaffold (~3.7 MB); point the app at a library later.
#
# Existing bgm/ files inside the package are preserved. For a clean rebuild, delete
# dist\tfy-bgm manually. -EmptyBgm refuses to run if the existing bgm/ has mp3 files.
# -WithBgm is kept for compatibility and is equivalent to the default.
# The bgm/ folder is never embedded into the executable.

param(
    [switch]$WithBgm,
    [switch]$EmptyBgm,
    [string]$OutputDir = "dist"
)

$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent $PSScriptRoot
$srcTauri = Join-Path $repo "src-tauri"
$releaseExe = Join-Path $srcTauri "target\release\tfy-bgm.exe"
$bgmSource = Join-Path $repo "bgm"

$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) {
    $cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
}
if (-not (Test-Path $cargo)) {
    throw "cargo not found. Install Rust from https://rustup.rs and reopen the terminal."
}

$package = Join-Path (Join-Path $repo $OutputDir) "tfy-bgm"
$running = Get-Process tfy-bgm -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($package, [StringComparison]::OrdinalIgnoreCase) }
if ($running) {
    throw "tfy-bgm.exe is running from $package. Close it before packaging."
}

Write-Host "[1/3] cargo build --release"
Push-Location $srcTauri
try {
    & $cargo build --release
} finally {
    Pop-Location
}

if (-not (Test-Path $releaseExe)) {
    throw "Release executable not found: $releaseExe"
}

Write-Host "[2/3] assemble portable folder"
New-Item -ItemType Directory -Force $package | Out-Null
Copy-Item $releaseExe $package -Force
Copy-Item (Join-Path $repo "LICENSE") $package -Force
Copy-Item (Join-Path $repo "README.md") $package -Force

$bgmTarget = Join-Path $package "bgm"
$existingMp3 = 0
if (Test-Path $bgmTarget) {
    $existingMp3 = @(Get-ChildItem $bgmTarget -Recurse -File -Filter *.mp3).Count
}

if ($EmptyBgm -and -not $WithBgm) {
    if ($existingMp3 -gt 0) {
        throw "Existing bgm/ contains $existingMp3 mp3 files. Move it or choose another -OutputDir before building an empty package."
    }
    New-Item -ItemType Directory -Force $bgmTarget | Out-Null
    $categories = Get-Content (Join-Path $srcTauri "core\assets\categories.default.json") -Raw | ConvertFrom-Json
    foreach ($category in $categories) {
        New-Item -ItemType Directory -Force (Join-Path $bgmTarget $category.id) | Out-Null
    }
    Write-Host "bgm/: empty category scaffold"
} elseif (Test-Path $bgmSource) {
    if ($existingMp3 -gt 0) {
        Write-Host "bgm/: kept existing $existingMp3 mp3 files (not overwritten)"
    } else {
        Copy-Item $bgmSource $bgmTarget -Recurse -Force
        Write-Host "bgm/: copied from $bgmSource"
    }
} else {
    New-Item -ItemType Directory -Force $bgmTarget | Out-Null
    $categories = Get-Content (Join-Path $srcTauri "core\assets\categories.default.json") -Raw | ConvertFrom-Json
    foreach ($category in $categories) {
        New-Item -ItemType Directory -Force (Join-Path $bgmTarget $category.id) | Out-Null
    }
    Write-Host "bgm/: repository bgm/ not found; empty category scaffold"
}

Write-Host "[3/3] done"
Write-Host "Portable package: $package"
Write-Host "Layout: tfy-bgm.exe + bgm\<category>\*.mp3"
$bytes = (Get-ChildItem $package -Recurse -File | Measure-Object -Property Length -Sum).Sum
Write-Host ("Package size: {0:N1} MB" -f ($bytes / 1MB))
Write-Host "Zip the tfy-bgm folder to distribute it."
