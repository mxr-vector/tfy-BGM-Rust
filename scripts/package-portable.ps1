# Build the Windows portable package (Windows PowerShell 5.1+).
#
# Usage (from the repository root):
#   powershell -ExecutionPolicy Bypass -File scripts/package-portable.ps1 [-Target TRIPLE] [-OutputDir DIR] [-SkipBuild] [-NoZip] [-WithBgm | -EmptyBgm]
# Target defaults to the rustc host in src-tauri. A ZIP is always built from
# clean staging. -NoZip assembles OutputDir\tfy-bgm in place instead and
# preserves a custom library. -WithBgm uses the repository's bgm/ contents;
# -EmptyBgm is an alias for the default empty category scaffold.

[CmdletBinding(PositionalBinding = $false)]
param(
    [switch]$WithBgm,
    [switch]$EmptyBgm,
    [string]$OutputDir = "dist",
    [switch]$SkipBuild,
    [switch]$NoZip,
    [ValidateNotNullOrEmpty()][string]$Target
)

$ErrorActionPreference = "Stop"
if ($WithBgm -and $EmptyBgm) { throw "Specify only one of -WithBgm and -EmptyBgm" }
. (Join-Path $PSScriptRoot "package-common.ps1")

function Copy-BgmContents {
    param([string]$Source, [string]$Destination)

    [System.IO.Directory]::CreateDirectory($Destination) | Out-Null
    foreach ($item in Get-ChildItem -LiteralPath $Source -Force) {
        $dest = Join-Path $Destination $item.Name
        if ($item.PSIsContainer) {
            if (-not (Test-Path -LiteralPath $dest) -or (Test-Path -LiteralPath $dest -PathType Container)) {
                Copy-BgmContents -Source $item.FullName -Destination $dest
            }
        } elseif (-not (Test-Path -LiteralPath $dest)) {
            Copy-Item -LiteralPath $item.FullName -Destination $dest
        }
    }
}

function Initialize-PortableBgm {
    param([string]$Destination, [string]$Source, [string]$CategoriesPath, [switch]$IncludeLibrary)

    if (Test-Path -LiteralPath $Destination) {
        # Treat only a scaffold (empty directories and .gitkeep placeholders)
        # as eligible to receive repository files. Leave a custom library alone.
        $existing = Get-Item -LiteralPath $Destination -Force
        if (-not $existing.PSIsContainer -or ($existing.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            (Get-ChildItem -LiteralPath $Destination -Force -Recurse | Where-Object {
                ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -or (-not $_.PSIsContainer -and $_.Name -ne '.gitkeep')
            } | Select-Object -First 1)) {
            Write-Host "bgm/: kept existing contents in $Destination"
            return
        }
    }

    if ($IncludeLibrary -and (Test-Path -LiteralPath $Source -PathType Container)) {
        Copy-BgmContents -Source $Source -Destination $Destination
        Write-Host "bgm/: copied contents from $Source"
    } else {
        if ($IncludeLibrary) { Write-Warning "Repository bgm/ not found; using the empty scaffold" }
        [System.IO.Directory]::CreateDirectory($Destination) | Out-Null
        $categories = Get-Content -LiteralPath $CategoriesPath -Raw -Encoding UTF8 | ConvertFrom-Json
        foreach ($category in $categories) {
            [System.IO.Directory]::CreateDirectory((Join-Path $Destination $category.id)) | Out-Null
        }
        Write-Host "bgm/: empty category scaffold"
    }
}

function New-PortableZip {
    param([string]$Package, [string]$Path)

    Add-Type -AssemblyName System.IO.Compression
    # Write forward-slash paths and include directory entries for the empty scaffold.
    $level = [System.IO.Compression.CompressionLevel]::Fastest
    $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::CreateNew)
    try {
        $archive = New-Object System.IO.Compression.ZipArchive(
            $stream, [System.IO.Compression.ZipArchiveMode]::Create, $true)
        try {
            foreach ($item in Get-ChildItem -LiteralPath $Package -Recurse -Force) {
                $rel = $item.FullName.Substring($Package.Length + 1).Replace("\", "/")
                if ($item.PSIsContainer) {
                    $archive.CreateEntry("tfy-bgm/$rel/") | Out-Null
                    continue
                }
                $entry = $archive.CreateEntry("tfy-bgm/$rel", $level)
                $sourceStream = [System.IO.File]::OpenRead($item.FullName)
                try {
                    $entryStream = $entry.Open()
                    try { $sourceStream.CopyTo($entryStream) } finally { $entryStream.Dispose() }
                } finally {
                    $sourceStream.Dispose()
                }
            }
        } finally {
            $archive.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

$repo = Get-RepoRoot -ScriptRoot $PSScriptRoot
$srcTauri = Join-Path $repo "src-tauri"
$cargo = Resolve-CargoExe
$context = Get-WindowsPackageContext -SrcTauri $srcTauri -Cargo $cargo -Target $Target
$version = $context.Version
$arch = $context.Arch
$releaseExe = Join-Path $context.ReleaseDir "tfy-bgm.exe"
$dist = Resolve-PackageOutputDir -RepoRoot $repo -OutputDir $OutputDir
$package = Join-Path $dist "tfy-bgm"
$zipName = "tfy-bgm-$version-windows-$arch-portable.zip"
$zipPath = Join-Path $dist $zipName

if ($NoZip) { Assert-NotRunning -Path $package }

if (-not $SkipBuild) {
    # cargo build does not run Tauri's beforeBuildCommand.
    if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) {
        throw "pnpm not found. Install Node.js 20+ and run: corepack enable"
    }
    Write-Host "[1/4] pnpm run build + cargo build --release --target $($context.Target)"
    Initialize-SherpaNativeLibs -RepoRoot $repo -TargetArch $arch | Out-Null
    Push-Location -LiteralPath $repo
    try {
        & pnpm run build
        if ($LASTEXITCODE -ne 0) { throw "frontend build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
    Push-Location -LiteralPath $srcTauri
    try {
        # Enable the embedded frontend instead of connecting to devUrl.
        & $cargo build --release --target $context.Target --features custom-protocol
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
} else {
    Write-Host "[1/4] build skipped (-SkipBuild)"
}

Assert-PeArchitecture -Path $releaseExe -Machine $context.Machine
[System.IO.Directory]::CreateDirectory($dist) | Out-Null
$staging = $null
try {
    if (-not $NoZip) {
        $staging = Join-Path $dist (".portable-" + [Guid]::NewGuid().ToString("N"))
        [System.IO.Directory]::CreateDirectory($staging) | Out-Null
        $package = Join-Path $staging "tfy-bgm"
    }
    Write-Host "[2/4] assemble portable folder"
    [System.IO.Directory]::CreateDirectory($package) | Out-Null
    Copy-Item -LiteralPath $releaseExe, (Join-Path $repo "LICENSE"), (Join-Path $repo "README.md") -Destination $package -Force

    # Resources resolve relative to the executable in the portable build:
    # assets/bg (wallpapers), voices (TTS timbres) and models (separation ONNX +
    # Spleeter archive) must all sit next to tfy-bgm.exe.
    $assets = Join-Path $package "assets"
    [System.IO.Directory]::CreateDirectory($assets) | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo "assets\bg") -Destination $assets -Recurse -Force
    Copy-Item -LiteralPath (Join-Path $repo "models\voices") -Destination $package -Recurse -Force
    Copy-BundledSeparationModels -RepoRoot $repo -Destination (Join-Path $package "models") | Out-Null
    Initialize-PortableBgm -Destination (Join-Path $package "bgm") -Source (Join-Path $repo "bgm") `
        -CategoriesPath (Join-Path $srcTauri "core\assets\categories.default.json") -IncludeLibrary:$WithBgm
    Assert-PortableResources -Package $package

    if ($NoZip) {
        Write-Host "[3/4] zip skipped (-NoZip)"
    } else {
        Write-Host "[3/4] zip -> $zipName"
        $stagedZip = Join-Path $staging $zipName
        New-PortableZip -Package $package -Path $stagedZip
        Move-Item -LiteralPath $stagedZip -Destination $zipPath -Force
    }
} finally {
    if ($staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
}

Write-Host "[4/4] checksums"
Write-Checksums -DistDir $dist -Version $version
if ($NoZip) {
    Write-Host "Folder: $package"
} else {
    Write-Host ("Archive: $zipPath ({0})" -f (Format-FileSize (Get-Item -LiteralPath $zipPath).Length))
}
