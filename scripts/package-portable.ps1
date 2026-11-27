# Build a portable Windows package: tfy-bgm.exe + an external bgm/ folder,
# then zip it as tfy-bgm-<version>-windows-<arch>-portable.zip.
#
# Usage (from the repository root):
#   powershell -ExecutionPolicy Bypass -File scripts/package-portable.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/package-portable.ps1 -WithBgm
#   powershell -ExecutionPolicy Bypass -File scripts/package-portable.ps1 -SkipBuild -NoZip
#
# Default: no audio. Only an empty category scaffold ships in bgm/ (package ~34 MB,
# mostly built-in wallpapers and voice prompts), and the app points at a library
# chosen on first use.
# -WithBgm: copy the repository bgm/ folder next to the exe (self-contained, ~350 MB).
#
# Existing bgm/ files inside the package are preserved. For a clean rebuild, delete
# dist\tfy-bgm manually. The default (no-audio) build refuses to run if the existing
# bgm/ already has audio files, so a library is never shipped by accident; pass
# -WithBgm to keep those files.
# -EmptyBgm is kept for compatibility and is equivalent to the default.
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
    # cargo build does not run tauri's beforeBuildCommand, so build the frontend first.
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
        # custom-protocol must be enabled, otherwise Tauri treats the app as dev mode
        # and loads devUrl instead of the embedded frontend, leaving the portable exe blank.
        & $cargo build --release --features custom-protocol
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
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

# Built-in wallpapers resolve via resource_dir (the exe folder for the portable build),
# so assets/bg has to ship next to the executable.
New-Item -ItemType Directory -Force (Join-Path $package "assets") | Out-Null
Copy-Item (Join-Path $repo "assets\bg") (Join-Path $package "assets") -Recurse -Force
$voicesTarget = Join-Path $package "voices"
New-Item -ItemType Directory -Force $voicesTarget | Out-Null
Copy-Item (Join-Path $repo "models\voices\*.wav") $voicesTarget -Force

$bgmTarget = Join-Path $package "bgm"
$audioExtensions = @(".mp3", ".m4a", ".flac", ".wav", ".ogg")
$existingAudio = 0
if (Test-Path $bgmTarget) {
    $existingAudio = @(
        Get-ChildItem $bgmTarget -Recurse -File |
            Where-Object { $audioExtensions -contains $_.Extension.ToLowerInvariant() }
    ).Count
}

# An empty bgm/ still needs the category folders so the first launch scans cleanly.
function New-EmptyBgmScaffold([string]$Path) {
    New-Item -ItemType Directory -Force $Path | Out-Null
    $categories = Get-Content (Join-Path $srcTauri "core\assets\categories.default.json") -Raw -Encoding UTF8 | ConvertFrom-Json
    foreach ($category in $categories) {
        New-Item -ItemType Directory -Force (Join-Path $Path $category.id) | Out-Null
    }
}

if (-not $WithBgm) {
    if ($existingAudio -gt 0) {
        throw "Existing bgm/ contains $existingAudio audio files. Delete $bgmTarget for a clean no-audio build, or pass -WithBgm to keep them."
    }
    New-EmptyBgmScaffold $bgmTarget
    Write-Host "bgm/: empty category scaffold (no audio; pass -WithBgm to bundle the repository library)"
} elseif (Test-Path $bgmSource) {
    if ($existingAudio -gt 0) {
        Write-Host "bgm/: kept existing $existingAudio audio files (not overwritten)"
    } else {
        Copy-Item $bgmSource $bgmTarget -Recurse -Force
        Write-Host "bgm/: copied from $bgmSource"
    }
} else {
    New-EmptyBgmScaffold $bgmTarget
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
    # CompressionLevel.Fastest: the payload is mostly already-compressed audio,
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
Write-Host "Layout : tfy-bgm.exe + bgm\<category>\*.{mp3,m4a,flac,wav,ogg}"
if (Test-Path $zipPath) {
    Write-Host ("Archive: $zipPath ({0})" -f (Format-FileSize (Get-Item $zipPath).Length))
    Write-Host "Upload the .zip; users extract it and run tfy-bgm.exe."
} else {
    Write-Host "Zip the tfy-bgm folder to distribute it."
}
