# Build the Windows NSIS installer and copy it to dist/ with a canonical name.
#
# Usage (from the repository root):
#   powershell -ExecutionPolicy Bypass -File scripts/package-installer.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/package-installer.ps1 -SkipBuild
#
# Output: dist\tfy-bgm-<version>-windows-<arch>-setup.exe
# The installer does not contain audio; the app creates bgm/ next to the exe on
# first launch (or points at a library via the UI).
#
# Requires tauri-cli: cargo install tauri-cli --version "^2" --locked

param(
    [string]$OutputDir = "dist",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "package-common.ps1")

$repo = Get-RepoRoot -ScriptRoot $PSScriptRoot
$version = Get-AppVersion -RepoRoot $repo
$arch = Get-WindowsArch
$srcTauri = Join-Path $repo "src-tauri"
$dist = Join-Path $repo $OutputDir
$bundleDir = Join-Path $srcTauri "target\release\bundle\nsis"

$targetName = "tfy-bgm-$version-windows-$arch-setup.exe"
$targetPath = Join-Path $dist $targetName

if (-not $SkipBuild) {
    $cargo = Resolve-CargoExe
    Write-Host "[1/3] cargo tauri build --bundles nsis"
    Initialize-SherpaNativeLibs -RepoRoot $repo | Out-Null
    Push-Location $srcTauri
    try {
        & $cargo tauri --version | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "tauri-cli not found. Run: cargo install tauri-cli --version `"^2`" --locked"
        }
        & $cargo tauri build --bundles nsis
        if ($LASTEXITCODE -ne 0) { throw "cargo tauri build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
} else {
    Write-Host "[1/3] build skipped (-SkipBuild)"
}

Write-Host "[2/3] collect installer"
if (-not (Test-Path $bundleDir)) {
    throw "NSIS output not found: $bundleDir. Run without -SkipBuild first."
}
$candidates = @(Get-ChildItem $bundleDir -File -Filter "*-setup.exe")
if ($candidates.Count -eq 0) {
    throw "No *-setup.exe found in $bundleDir. Run without -SkipBuild."
}
# Never relabel a stale build: the artifact must carry the current version.
$artifact = $candidates |
    Where-Object { $_.Name -like "*_$($version)_*-setup.exe" } |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
if (-not $artifact) {
    throw "No installer for version $version in $bundleDir (found: $($candidates.Name -join ', ')). Rebuild without -SkipBuild."
}

New-Item -ItemType Directory -Force $dist | Out-Null
Copy-Item $artifact.FullName $targetPath -Force
# Older setup.exe files in dist/ are left untouched; SHA256SUMS.txt only covers
# the artifacts of the current version.

Write-Host "[3/3] checksums"
Write-Checksums -DistDir $dist -Version $version

Write-Host "[done]"
Write-Host ("Installer: $targetPath ({0})" -f (Format-FileSize (Get-Item $targetPath).Length))
Write-Host "Note: this package has no audio; bgm/ is created on first launch."
