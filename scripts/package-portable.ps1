# Build a portable Windows package: tfy-bgm.exe + an external bgm/ folder,
# then zip it as tfy-bgm-<version>-windows-<arch>-portable.zip.
#
# Usage (from the repository root):
#   powershell -ExecutionPolicy Bypass -File scripts/package-portable.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/package-portable.ps1 -EmptyBgm
#   powershell -ExecutionPolicy Bypass -File scripts/package-portable.ps1 -SkipBuild -NoZip
#
# Default: copy the repository bgm/ folder next to the exe (self-contained, ~350 MB).
# -EmptyBgm: create an empty category scaffold (~3.7 MB); point the app at a library later.
#
# Existing bgm/ files inside the package are preserved. For a clean rebuild, delete
# dist\tfy-bgm manually. -EmptyBgm refuses to run if the existing bgm/ has mp3 files.
# -WithBgm is kept for compatibility and is equivalent to the default.
# -SkipBuild reuses src-tauri\target\release\tfy-bgm.exe. -NoZip only assembles the folder.
# The bgm/ folder is never embedded into the executable.

param(
    [switch]$WithBgm,
    [switch]$EmptyBgm,
    [string]$OutputDir = "dist",
    [switch]$SkipBuild,
    [switch]$NoZip
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "package-common.ps1")

$repo = Get-RepoRoot -ScriptRoot $PSScriptRoot
$version = Get-AppVersion -RepoRoot $repo
$arch = Get-WindowsArch
$srcTauri = Join-Path $repo "src-tauri"
$releaseExe = Join-Path $srcTauri "target\release\tfy-bgm.exe"
$bgmSource = Join-Path $repo "bgm"

$dist = Join-Path $repo $OutputDir
$package = Join-Path $dist "tfy-bgm"
$zipName = "tfy-bgm-$version-windows-$arch-portable.zip"
$zipPath = Join-Path $dist $zipName

Assert-NotRunning -Path $package

if (-not $SkipBuild) {
    # cargo build 不会执行 tauri 的 beforeBuildCommand，先单独构建前端产物。
    if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) {
        throw "pnpm not found. Install Node.js 20+ and run: corepack enable"
    }
    $cargo = Resolve-CargoExe
    Write-Host "[1/4] pnpm run build + cargo build --release"
    Push-Location $repo
    try {
        & pnpm run build
        if ($LASTEXITCODE -ne 0) { throw "frontend build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
    Push-Location $srcTauri
    try {
        & $cargo build --release
    } finally {
        Pop-Location
    }
} else {
    Write-Host "[1/4] build skipped (-SkipBuild)"
}

if (-not (Test-Path $releaseExe)) {
    throw "Release executable not found: $releaseExe"
}

Write-Host "[2/4] assemble portable folder"
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
    $categories = Get-Content (Join-Path $srcTauri "core\assets\categories.default.json") -Raw -Encoding UTF8 | ConvertFrom-Json
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
    $categories = Get-Content (Join-Path $srcTauri "core\assets\categories.default.json") -Raw -Encoding UTF8 | ConvertFrom-Json
    foreach ($category in $categories) {
        New-Item -ItemType Directory -Force (Join-Path $bgmTarget $category.id) | Out-Null
    }
    Write-Host "bgm/: repository bgm/ not found; empty category scaffold"
}

if ($NoZip) {
    Write-Host "[3/4] zip skipped (-NoZip)"
} else {
    Write-Host "[3/4] zip -> $zipName"
    if (-not ('System.IO.Compression.ZipArchive' -as [type])) {
        Add-Type -AssemblyName System.IO.Compression
        Add-Type -AssemblyName System.IO.Compression.FileSystem
    }
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }

    # Built by hand instead of ZipFile::CreateFromDirectory: on .NET Framework it
    # writes entry names with "\", which breaks unzip on Linux/macOS. Entries must
    # use "/". Directories are added explicitly so an empty bgm/ scaffold survives.
    #
    # CompressionLevel.Fastest: the payload is mostly already-compressed mp3,
    # Optimal only costs time.
    $level = [System.IO.Compression.CompressionLevel]::Fastest
    $rootName = "tfy-bgm"
    $stream = [System.IO.File]::Open($zipPath, [System.IO.FileMode]::Create)
    try {
        $archive = New-Object System.IO.Compression.ZipArchive(
            $stream, [System.IO.Compression.ZipArchiveMode]::Create, $true)
        try {
            foreach ($dir in Get-ChildItem $package -Recurse -Directory) {
                $rel = $dir.FullName.Substring($package.Length + 1).Replace("\", "/")
                $archive.CreateEntry("$rootName/$rel/") | Out-Null
            }
            foreach ($file in Get-ChildItem $package -Recurse -File) {
                $rel = $file.FullName.Substring($package.Length + 1).Replace("\", "/")
                $entry = $archive.CreateEntry("$rootName/$rel", $level)
                $input = [System.IO.File]::OpenRead($file.FullName)
                try {
                    $output = $entry.Open()
                    try { $input.CopyTo($output) } finally { $output.Dispose() }
                } finally {
                    $input.Dispose()
                }
            }
        } finally {
            $archive.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

Write-Host "[4/4] checksums"
Write-Checksums -DistDir $dist -Version $version

Write-Host "[done]"
Write-Host "Folder : $package"
Write-Host "Layout : tfy-bgm.exe + bgm\<category>\*.mp3"
if (Test-Path $zipPath) {
    Write-Host ("Archive: $zipPath ({0})" -f (Format-FileSize (Get-Item $zipPath).Length))
    Write-Host "Upload the .zip; users extract it and run tfy-bgm.exe."
} else {
    Write-Host "Zip the tfy-bgm folder to distribute it."
}
