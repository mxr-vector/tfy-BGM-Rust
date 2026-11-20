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
