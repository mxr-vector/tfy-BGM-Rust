# Build the Windows NSIS installer and copy it to dist/ with a canonical name.
#
# Usage (from the repository root):
#   powershell -ExecutionPolicy Bypass -File scripts/package-installer.ps1 [-Target TRIPLE] [-OutputDir DIR] [-SkipBuild]
# Target defaults to the rustc host in src-tauri. Requires Tauri CLI 2 to build.

[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$OutputDir = "dist",
    [switch]$SkipBuild,
    [ValidateNotNullOrEmpty()][string]$Target
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "package-common.ps1")

$repo = Get-RepoRoot -ScriptRoot $PSScriptRoot
$srcTauri = Join-Path $repo "src-tauri"
$cargo = Resolve-CargoExe
$context = Get-WindowsPackageContext -SrcTauri $srcTauri -Cargo $cargo -Target $Target
$version = $context.Version
$arch = $context.Arch
$dist = Resolve-PackageOutputDir -RepoRoot $repo -OutputDir $OutputDir
$releaseExe = Join-Path $context.ReleaseDir "tfy-bgm.exe"
$artifact = Join-Path $context.ReleaseDir "bundle\nsis\tfy-bgm_${version}_${arch}-setup.exe"
$targetPath = Join-Path $dist "tfy-bgm-$version-windows-$arch-setup.exe"

if (-not $SkipBuild) {
    Write-Host "[1/3] cargo tauri build --target $($context.Target) --bundles nsis"
    Initialize-SherpaNativeLibs -RepoRoot $repo -TargetArch $arch | Out-Null
    Push-Location -LiteralPath $srcTauri
    try {
        & $cargo tauri --version | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "tauri-cli not found. Run: cargo install tauri-cli --version `"^2`" --locked"
        }
        & $cargo tauri build --target $context.Target --bundles nsis
        if ($LASTEXITCODE -ne 0) { throw "cargo tauri build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
} else {
    Write-Host "[1/3] build skipped (-SkipBuild)"
}

Write-Host "[2/3] verify and collect installer"
# The NSIS bootstrapper may use a different machine type than the actual app.
Assert-PeArchitecture -Path $releaseExe -Machine $context.Machine
if (-not (Test-Path -LiteralPath $artifact -PathType Leaf) -or
    (Get-Item -LiteralPath $artifact).Name -cne [System.IO.Path]::GetFileName($artifact)) {
    throw "Expected NSIS installer not found: $artifact. Run without -SkipBuild first."
}

[System.IO.Directory]::CreateDirectory($dist) | Out-Null
Copy-Item -LiteralPath $artifact -Destination $targetPath -Force

Write-Host "[3/3] checksums"
Write-Checksums -DistDir $dist -Version $version
Write-Host ("Installer: $targetPath ({0})" -f (Format-FileSize (Get-Item -LiteralPath $targetPath).Length))
