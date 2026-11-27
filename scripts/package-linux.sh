#!/usr/bin/env bash
# Build Linux packages (deb / rpm) and copy them to dist/ with canonical names.
#
# Usage (from the repository root):
#   bash scripts/package-linux.sh              # auto-detect: whatever the host can build
#   bash scripts/package-linux.sh deb rpm      # explicit formats
#   SKIP_BUILD=1 bash scripts/package-linux.sh rpm
#
# Output:
#   dist/tfy-bgm-<version>-linux-amd64.deb     (Architecture: amd64)
#   dist/tfy-bgm-<version>-linux-x86_64.rpm    (Release: 1 inside the RPM header)
#   dist/SHA256SUMS.txt
#
# Notes:
#   - deb/rpm cannot be cross-built from Windows; run this on Linux.
#   - Neither needs dpkg-deb, rpmbuild, nor Fedora: tauri bundles both in pure Rust
#     (flate2 + tar for deb, the `rpm` crate for rpm). Only the Tauri build
#     dependencies (WebKitGTK etc.) are required.
#   - Requires tauri-cli: cargo install tauri-cli --version "^2" --locked
#   - The rpm file name drops the release field on purpose (uniform 4 fields);
#     the package metadata still keeps Release: 1.
#   - deb/rpm include bgm/ through tauri.conf.json; the app reads it from the
#     resource directory.

set -euo pipefail

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
conf="$repo/src-tauri/tauri.conf.json"
dist="$repo/dist"

[ -f "$conf" ] || die "tauri.conf.json not found: $conf"

# tauri.conf.json is authoritative for the bundler; Cargo.toml must agree.
version="$(sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$conf" | head -n1)"
[ -n "$version" ] || die "cannot read version from $conf"

cargo_version="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$repo/src-tauri/Cargo.toml" | head -n1)"
if [ -n "$cargo_version" ] && [ "$cargo_version" != "$version" ]; then
    die "version mismatch: tauri.conf.json=$version, Cargo.toml=$cargo_version"
fi

case "$(uname -m)" in
    x86_64|amd64)  deb_arch=amd64;  rpm_arch=x86_64  ;;
    aarch64|arm64) deb_arch=arm64;  rpm_arch=aarch64 ;;
    *) die "unsupported architecture: $(uname -m)" ;;
esac

formats=("$@")
if [ ${#formats[@]} -eq 0 ]; then
    # Both formats are written by tauri's own pure-Rust bundlers, so any distro
    # that can build the app can also produce both packages.
    formats=(deb rpm)
fi

for fmt in "${formats[@]}"; do
    case "$fmt" in
        deb|rpm) ;;
        *) die "unknown format: $fmt (expected deb or rpm)" ;;
    esac
done

bundle_arg="$(IFS=,; echo "${formats[*]}")"

if [ "${SKIP_BUILD:-0}" = "1" ]; then
    echo "[1/3] build skipped (SKIP_BUILD=1)"
else
    command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust from https://rustup.rs"
    echo "[1/3] cargo tauri build --bundles $bundle_arg"

    # sherpa-onnx-sys downloads its native archive in the build script without
    # retry and without checking the result; prefetch + verify it here instead
    # (see the .mjs header).
    case "$(uname -m)" in
        x86_64|amd64)  sherpa_arch=x64   ;;
        aarch64|arm64) sherpa_arch=arm64 ;;
        *) die "unsupported architecture: $(uname -m)" ;;
    esac
    if command -v node >/dev/null 2>&1; then
        SHERPA_ONNX_LIB_DIR="$(node "$repo/scripts/fetch-sherpa-libs.mjs" \
            --target-os linux --target-arch "$sherpa_arch")" \
            || die "failed to prepare sherpa-onnx native libs"
        export SHERPA_ONNX_LIB_DIR
        echo "sherpa-onnx libs: $SHERPA_ONNX_LIB_DIR"
    else
        echo "warning: node not found; sherpa-onnx downloads its native libs itself (unverified)" >&2
    fi

    ( cd "$repo/src-tauri" && cargo tauri --version >/dev/null 2>&1 ) \
        || die "tauri-cli not found. Run: cargo install tauri-cli --version \"^2\" --locked"
    ( cd "$repo/src-tauri" && cargo tauri build --bundles "$bundle_arg" )
fi

echo "[2/3] collect artifacts"
mkdir -p "$dist"

newest() {
    # $1 = directory, $2 = file glob (already version-scoped)
    # Never relabel a stale build: the glob embeds the current version.
    local dir="$1" pattern="$2"
    [ -d "$dir" ] || return 0
    find "$dir" -maxdepth 1 -type f -name "$pattern" -printf '%T@ %p\n' 2>/dev/null \
        | sort -rn | head -n1 | cut -d' ' -f2-
}

copy_artifact() {
    local fmt="$1" src target
    case "$fmt" in
        deb)
            src="$(newest "$repo/src-tauri/target/release/bundle/deb" "*_${version}_${deb_arch}.deb")"
            target="$dist/tfy-bgm-$version-linux-$deb_arch.deb"
            ;;
        rpm)
            src="$(newest "$repo/src-tauri/target/release/bundle/rpm" "*-${version}-*.${rpm_arch}.rpm")"
            target="$dist/tfy-bgm-$version-linux-$rpm_arch.rpm"
            ;;
    esac
    [ -n "$src" ] || die "no $fmt artifact for version $version under src-tauri/target/release/bundle/$fmt (stale build only? rerun without SKIP_BUILD=1)"
    cp -f "$src" "$target"
    printf '  %s  <- %s\n' "$(basename "$target")" "${src#"$repo"/}"
}

for fmt in "${formats[@]}"; do
    copy_artifact "$fmt"
done

echo "[3/3] checksums"
# sha256sum output ("<hash>  <name>") matches what scripts/package-*.ps1 writes,
# so a single SHA256SUMS.txt can cover Windows and Linux artifacts.
shopt -s nullglob
artifacts=("$dist"/tfy-bgm-"$version"-*)
if [ ${#artifacts[@]} -gt 0 ]; then
    ( cd "$dist" && sha256sum "${artifacts[@]##*/}" > SHA256SUMS.txt )
else
    echo "no artifacts for version $version in $dist; SHA256SUMS.txt not written"
fi

echo "[done]"
ls -lh "$dist"/tfy-bgm-"$version"-* 2>/dev/null || true
