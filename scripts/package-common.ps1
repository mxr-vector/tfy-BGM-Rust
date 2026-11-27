# Shared helpers for the packaging scripts (dot-source, do not run directly).
#
#   . (Join-Path $PSScriptRoot "package-common.ps1")
#
# Artifact naming (keep in sync with scripts/package-linux.sh and README):
#   tfy-bgm-<version>-windows-<arch>-portable.zip
#   tfy-bgm-<version>-windows-<arch>-setup.exe
#   tfy-bgm-<version>-linux-<arch>.deb          arch = amd64 | arm64
#   tfy-bgm-<version>-linux-<arch>.rpm          arch = x86_64 | aarch64
#
# Requires Windows PowerShell 5.1+ (no PS7-only syntax).

Set-StrictMode -Version Latest

function Get-RepoRoot {
    param([Parameter(Mandatory)][string]$ScriptRoot)
    return (Split-Path -Parent $ScriptRoot)
}

function Get-WindowsPackageContext {
    param(
        [Parameter(Mandatory)][string]$SrcTauri,
        [Parameter(Mandatory)][string]$Cargo,
        [string]$Target
    )

    Push-Location -LiteralPath $SrcTauri
    try {
        if (-not $Target) {
            $rustc = Get-Command rustc -ErrorAction SilentlyContinue
            if ($rustc) { $rustcPath = $rustc.Source }
            else { $rustcPath = Join-Path (Split-Path -Parent $Cargo) "rustc.exe" }
            if (-not (Test-Path -LiteralPath $rustcPath -PathType Leaf)) { throw "rustc not found" }
            $rustInfo = & $rustcPath -vV
            if ($LASTEXITCODE -ne 0) { throw "rustc -vV failed (exit $LASTEXITCODE)" }
            $hostLine = $rustInfo | Where-Object { $_ -cmatch '^host: ' } | Select-Object -First 1
            if (-not $hostLine) { throw "Cannot read host target from rustc -vV" }
            $Target = $hostLine.Substring(6).Trim()
        }

        switch -CaseSensitive ($Target) {
            "x86_64-pc-windows-msvc" { $arch = "x64"; $machine = 0x8664 }
            "aarch64-pc-windows-msvc" { $arch = "arm64"; $machine = 0xAA64 }
            default { throw "Unsupported Windows target: $Target (expected x86_64-pc-windows-msvc or aarch64-pc-windows-msvc)" }
        }

        $metadataJson = & $Cargo metadata --format-version 1 --no-deps
        if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed (exit $LASTEXITCODE)" }
        $metadata = $metadataJson | ConvertFrom-Json
    } finally {
        Pop-Location
    }

    $manifest = Join-Path $SrcTauri "Cargo.toml"
    $app = @($metadata.packages | Where-Object { $_.manifest_path -eq $manifest -and $_.name -ceq "tfy-bgm" })
    if ($app.Count -ne 1 -or -not $metadata.target_directory) {
        throw "Cannot resolve the app package or target directory from cargo metadata"
    }
    # Windows PowerShell 5.1 defaults to ANSI for files without a BOM.
    $config = Get-Content -LiteralPath (Join-Path $SrcTauri "tauri.conf.json") -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($config.PSObject.Properties.Match("version").Count -eq 0 -or $config.version -cne $app[0].version) {
        throw "Version mismatch: tauri.conf.json must specify the Cargo package version ($($app[0].version))"
    }

    return [pscustomobject]@{
        Target = $Target
        Arch = $arch
        Machine = $machine
        Version = $config.version
        ReleaseDir = Join-Path $metadata.target_directory "$Target\release"
    }
}

function Resolve-PackageOutputDir {
    param([string]$RepoRoot, [string]$OutputDir)

    if (-not [System.IO.Path]::IsPathRooted($OutputDir)) { $OutputDir = Join-Path $RepoRoot $OutputDir }
    return [System.IO.Path]::GetFullPath($OutputDir)
}

function Assert-PeArchitecture {
    param([string]$Path, [int]$Machine)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Release executable not found: $Path" }
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $reader = New-Object System.IO.BinaryReader($stream)
        try {
            if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) {
                throw "Invalid PE header: $Path"
            }
            $stream.Position = 0x3C
            $peOffset = $reader.ReadUInt32()
            if ($peOffset -lt 64 -or $peOffset -gt ($stream.Length - 24)) { throw "Invalid PE offset: $Path" }
            $stream.Position = $peOffset
            if ($reader.ReadUInt32() -ne 0x4550) { throw "Invalid PE signature: $Path" }
            $actual = $reader.ReadUInt16()
            if ($actual -ne $Machine) {
                throw ("PE architecture mismatch in {0}: expected 0x{1:X4}, found 0x{2:X4}" -f $Path, $Machine, $actual)
            }
        } finally {
            $reader.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

function Resolve-CargoExe {
    # Strict mode: $null.Source throws, so resolve the command before touching .Source.
    $cargoCmd = Get-Command cargo -ErrorAction SilentlyContinue
    $cargo = $null
    if ($cargoCmd) { $cargo = $cargoCmd.Source }
    if (-not $cargo) {
        $cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    }
    if (-not (Test-Path $cargo)) {
        throw "cargo not found. Install Rust from https://rustup.rs and reopen the terminal."
    }
    return $cargo
}

# Refuse to overwrite an artifact while the packaged app is running from it.
function Assert-NotRunning {
    param([Parameter(Mandatory)][string]$Path)

    $exe = Join-Path $Path "tfy-bgm.exe"
    $running = Get-Process "tfy-bgm" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.Equals($exe, [StringComparison]::OrdinalIgnoreCase) }
    if ($running) {
        throw "tfy-bgm.exe is running from $Path. Close it before packaging."
    }
}

# Rewrite dist\SHA256SUMS.txt for the current version only (sha256sum -c format,
# two spaces, lowercase hex, LF endings, no BOM).
function Write-Checksums {
    param(
        [Parameter(Mandatory)][string]$DistDir,
        [Parameter(Mandatory)][string]$Version
    )

    $artifactPattern = '^tfy-bgm-' + [regex]::Escape($Version) + '-(windows-(x64|arm64)-(setup\.exe|portable\.zip)|linux-((amd64|arm64)\.deb|(x86_64|aarch64)\.rpm))$'
    $files = @(Get-ChildItem -LiteralPath $DistDir -File -Filter "tfy-bgm-$Version-*" |
        Where-Object { $_.Name -cmatch $artifactPattern } | Sort-Object Name)
    if ($files.Count -eq 0) {
        Write-Host "No artifacts found in $DistDir for version $Version; SHA256SUMS.txt not written."
        return
    }

    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $lines = foreach ($file in $files) {
            $stream = $file.OpenRead()
            try {
                $hash = [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-', '').ToLowerInvariant()
                "{0}  {1}" -f $hash, $file.Name
            } finally {
                $stream.Dispose()
            }
        }
    } finally {
        $sha.Dispose()
    }

    $target = Join-Path $DistDir "SHA256SUMS.txt"
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($target, (($lines -join "`n") + "`n"), $utf8NoBom)

    Write-Host "Checksums: $target ($($files.Count) file(s))"
}

function Format-FileSize {
    param([Parameter(Mandatory)][long]$Bytes)
    return ("{0:N1} MB" -f ($Bytes / 1MB))
}

# sherpa-onnx-sys downloads a ~127 MB native archive from GitHub Releases inside
# its build script (no retry, no mirror) and accepts any existing lib/ directory
# without checking the contents. On CI that ends in a late, confusing
# "could not find native static library `sherpa-onnx-c-api`".
#
# scripts/fetch-sherpa-libs.mjs prefetches the archive (retries + content
# check), extracts it outside target/, verifies all 13 libraries and prints the
# directory. Passing it via SHERPA_ONNX_LIB_DIR skips the download entirely
# and makes the build script re-run (it watches that variable), so a stale
# fingerprint can no longer point at an empty directory.
function Initialize-SherpaNativeLibs {
    param(
        [Parameter(Mandatory)][string]$RepoRoot,
        [Parameter(Mandatory)][string]$TargetArch
    )

    $fetcher = Join-Path $RepoRoot "scripts\fetch-sherpa-libs.mjs"
    $nodeCmd = Get-Command node -ErrorAction SilentlyContinue
    if (-not $nodeCmd -or -not (Test-Path $fetcher)) {
        Write-Warning "node or scripts/fetch-sherpa-libs.mjs missing; sherpa-onnx will download its native libs itself (unverified)"
        return $null
    }

    # Progress goes to the helper's stderr (shown as-is); only the verified lib
    # directory lands on stdout, so the last non-empty line is the path.
    $output = & $nodeCmd.Source $fetcher --target-os windows --target-arch $TargetArch
    $exit = $LASTEXITCODE
    if ($exit -ne 0) { throw "failed to prepare sherpa-onnx native libs (exit $exit)" }

    $libDir = @($output | Where-Object { $_ -and "$_".Trim() }) | Select-Object -Last 1
    if (-not $libDir) { throw "scripts/fetch-sherpa-libs.mjs returned no lib directory" }

    $env:SHERPA_ONNX_LIB_DIR = "$libDir".Trim()
    Write-Host "sherpa-onnx libs: $env:SHERPA_ONNX_LIB_DIR"
    return $env:SHERPA_ONNX_LIB_DIR
}
