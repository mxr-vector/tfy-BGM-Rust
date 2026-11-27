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

# Version is read from tauri.conf.json (authoritative for the bundler) and
# cross-checked against Cargo.toml so a stale Cargo version cannot silently
# produce a mislabelled artifact.
function Get-AppVersion {
    param([Parameter(Mandatory)][string]$RepoRoot)

    $conf = Join-Path $RepoRoot "src-tauri\tauri.conf.json"
    if (-not (Test-Path $conf)) { throw "tauri.conf.json not found: $conf" }

    # -Encoding UTF8 is required: Windows PowerShell 5.1 defaults to ANSI and
    # would mangle the Chinese descriptions in tauri.conf.json.
    $config = Get-Content $conf -Raw -Encoding UTF8 | ConvertFrom-Json
    $version = $null
    if ($config.PSObject.Properties.Match("version").Count -gt 0) { $version = $config.version }
    if (-not $version) { throw "tauri.conf.json has no version field" }

    # Only the first `version = "..."` in Cargo.toml is [package].version;
    # `rust-version` and dependency versions do not match this pattern.
    $cargoToml = Join-Path $RepoRoot "src-tauri\Cargo.toml"
    if (Test-Path $cargoToml) {
        $match = Select-String -Path $cargoToml -Pattern '^version\s*=\s*"([^"]+)"' |
            Select-Object -First 1
        if ($match) {
            $cargoVersion = $match.Matches[0].Groups[1].Value
            if ($cargoVersion -ne $version) {
                throw "Version mismatch: tauri.conf.json=$version, Cargo.toml=$cargoVersion. Align them before packaging."
            }
        }
    }

    return $version
}

function Get-WindowsArch {
    $arch = $null
    try {
        $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    } catch {
        $arch = $env:PROCESSOR_ARCHITECTURE
    }
    switch ($arch) {
        "X64" { return "x64" }
        "AMD64" { return "x64" }
        "Arm64" { return "arm64" }
        "ARM64" { return "arm64" }
        "X86" { return "x86" }
        "x86" { return "x86" }
        default { throw "Unsupported Windows architecture: $arch" }
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

    $running = Get-Process "tfy-bgm" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($Path, [StringComparison]::OrdinalIgnoreCase) }
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

    $files = @(Get-ChildItem $DistDir -File -Filter "tfy-bgm-$Version-*" | Sort-Object Name)
    if ($files.Count -eq 0) {
        Write-Host "No artifacts found in $DistDir for version $Version; SHA256SUMS.txt not written."
        return
    }

    $lines = foreach ($file in $files) {
        $hash = (Get-FileHash $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        "{0}  {1}" -f $hash, $file.Name
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
        [ValidateSet("windows", "linux")][string]$TargetOs = "windows",
        [string]$TargetArch = ""
    )

    if (-not $TargetArch) { $TargetArch = Get-WindowsArch }

    $fetcher = Join-Path $RepoRoot "scripts\fetch-sherpa-libs.mjs"
    $nodeCmd = Get-Command node -ErrorAction SilentlyContinue
    if (-not $nodeCmd -or -not (Test-Path $fetcher)) {
        Write-Warning "node or scripts/fetch-sherpa-libs.mjs missing; sherpa-onnx will download its native libs itself (unverified)"
        return $null
    }

    # Progress goes to the helper's stderr (shown as-is); only the verified lib
    # directory lands on stdout, so the last non-empty line is the path.
    $output = & $nodeCmd.Source $fetcher --target-os $TargetOs --target-arch $TargetArch
    $exit = $LASTEXITCODE
    if ($exit -ne 0) { throw "failed to prepare sherpa-onnx native libs (exit $exit)" }

    $libDir = @($output | Where-Object { $_ -and "$_".Trim() }) | Select-Object -Last 1
    if (-not $libDir) { throw "scripts/fetch-sherpa-libs.mjs returned no lib directory" }

    $env:SHERPA_ONNX_LIB_DIR = "$libDir".Trim()
    Write-Host "sherpa-onnx libs: $env:SHERPA_ONNX_LIB_DIR"
    return $env:SHERPA_ONNX_LIB_DIR
}
