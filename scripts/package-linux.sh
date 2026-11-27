#!/usr/bin/env bash
# Build Linux DEB/RPM packages and copy them to dist/ with canonical names.
# Usage: bash scripts/package-linux.sh [--target TRIPLE] [deb] [rpm]
# Default target: rustc host in src-tauri. Default formats: deb and rpm.
# SKIP_BUILD=1 collects existing target-specific build output.
# Requires Python 3 for JSON and dpkg-deb / rpm to inspect the selected formats.
# Building requires Tauri CLI 2 and the Linux build dependencies, not rpmbuild.

set -euo pipefail

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src_tauri="$repo/src-tauri"
dist="$repo/dist"

target=
if [[ ${1:-} == --target ]]; then
    [[ $# -ge 2 && -n $2 && $2 != -* ]] || die 'usage: package-linux.sh [--target TRIPLE] [deb] [rpm]'
    target=$2
    shift 2
fi
formats=("$@")
if [[ ${#formats[@]} -eq 0 ]]; then
    formats=(deb rpm)
fi
for fmt in "${formats[@]}"; do
    case "$fmt" in
        deb|rpm) ;;
        *) die "unknown format: $fmt (expected deb or rpm; --target TRIPLE must precede formats)" ;;
    esac
done

if [[ -z $target ]]; then
    rust_info="$(cd "$src_tauri" && rustc -vV)" || die 'rustc -vV failed'
    while IFS= read -r line; do
        if [[ $line == 'host: '* ]]; then
            target=${line#host: }
            break
        fi
    done <<< "$rust_info"
    [[ -n $target ]] || die 'cannot read host target from rustc -vV'
fi

case "$target" in
    x86_64-unknown-linux-gnu) deb_arch=amd64; rpm_arch=x86_64; sherpa_arch=x64; elf_machine='3e00' ;;
    aarch64-unknown-linux-gnu) deb_arch=arm64; rpm_arch=aarch64; sherpa_arch=arm64; elf_machine='b700' ;;
    *) die "unsupported Linux target: $target (expected x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu)" ;;
esac

for fmt in "${formats[@]}"; do
    tool=$fmt
    [[ $fmt != deb ]] || tool=dpkg-deb
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required to verify $fmt metadata"
done
command -v cargo >/dev/null 2>&1 || die 'cargo not found. Install Rust from https://rustup.rs'
command -v python3 >/dev/null 2>&1 || die 'python3 is required to read Cargo and Tauri metadata'

# Run Cargo here so the toolchain and target directory follow Cargo config.
metadata="$(cd "$src_tauri" && cargo metadata --format-version 1 --no-deps)" \
    || die 'cargo metadata failed'
package_info="$(python3 -c '
import json
import pathlib
import sys

src = pathlib.Path(sys.argv[1])
metadata = json.load(sys.stdin)
apps = [p for p in metadata["packages"] if p["name"] == "tfy-bgm"
        and pathlib.Path(p["manifest_path"]).resolve() == src / "Cargo.toml"]
if len(apps) != 1 or not metadata["target_directory"]:
    sys.exit("cannot resolve the app package or target directory from cargo metadata")
with (src / "tauri.conf.json").open(encoding="utf-8") as f:
    version = json.load(f).get("version")
cargo_version = apps[0]["version"]
if version != cargo_version:
    sys.exit(f"version mismatch: tauri.conf.json={version}, Cargo.toml={cargo_version}")
print(version)
print(metadata["target_directory"])
' "$src_tauri" <<< "$metadata")" || die 'cannot determine packaging version and target directory'
version=${package_info%%$'\n'*}
release_dir="${package_info#*$'\n'}/$target/release"
bundle_arg="$(IFS=,; printf '%s' "${formats[*]}")"

if [[ ${SKIP_BUILD:-0} == 1 ]]; then
    echo '[1/3] build skipped (SKIP_BUILD=1)'
else
    echo "[1/3] cargo tauri build --target $target --bundles $bundle_arg"
    if command -v node >/dev/null 2>&1; then
        SHERPA_ONNX_LIB_DIR="$(node "$repo/scripts/fetch-sherpa-libs.mjs" \
            --target-os linux --target-arch "$sherpa_arch")" || die 'failed to prepare sherpa-onnx native libs'
        export SHERPA_ONNX_LIB_DIR
        echo "sherpa-onnx libs: $SHERPA_ONNX_LIB_DIR"
    else
        echo 'warning: node not found; sherpa-onnx downloads its native libs itself (unverified)' >&2
    fi
    ( cd "$src_tauri" && cargo tauri --version >/dev/null 2>&1 ) \
        || die 'tauri-cli not found. Run: cargo install tauri-cli --version "^2" --locked'
    ( cd "$src_tauri" && cargo tauri build --target "$target" --bundles "$bundle_arg" )
fi

exe="$release_dir/tfy-bgm"
[[ -f $exe ]] || die "release executable not found: $exe"
# ELF magic, 64-bit class, little-endian encoding, and e_machine at byte 18.
elf_header="$(LC_ALL=C od -An -v -tx1 -N64 -- "$exe" | tr -d '[:space:]')"
[[ ${#elf_header} -eq 128 && ${elf_header:0:12} == 7f454c460201 ]] || die "invalid 64-bit little-endian ELF header: $exe"
[[ ${elf_header:36:4} == "$elf_machine" ]] \
    || die "ELF architecture mismatch in $exe for $target: e_machine bytes ${elf_header:36:4}"

# Tauri keeps SemVer in the RPM filename but normalizes it in the RPM header:
# prerelease starts with ~, and hyphens inside prerelease/build become dots.
rpm_version=$version
if [[ $version =~ ^([0-9]+\.[0-9]+\.[0-9]+)(-([0-9A-Za-z.-]+))?(\+([0-9A-Za-z.-]+))?$ ]]; then
    rpm_version=${BASH_REMATCH[1]}
    pre=${BASH_REMATCH[3]}
    build=${BASH_REMATCH[5]}
    if [[ -n $pre ]]; then rpm_version+="~${pre//-/.}"; fi
    if [[ -n $build ]]; then rpm_version+="+${build//-/.}"; fi
fi

echo '[2/3] verify and collect artifacts'
sources=()
destinations=()
for fmt in "${formats[@]}"; do
    case "$fmt" in
        deb)
            src="$release_dir/bundle/deb/tfy-bgm_${version}_${deb_arch}.deb"
            dest="tfy-bgm-$version-linux-$deb_arch.deb"
            [[ -f $src ]] || die "expected DEB not found: $src"
            name="$(dpkg-deb --field "$src" Package)" || die "cannot read DEB Package: $src"
            ver="$(dpkg-deb --field "$src" Version)" || die "cannot read DEB Version: $src"
            arch="$(dpkg-deb --field "$src" Architecture)" || die "cannot read DEB Architecture: $src"
            actual="$name $ver $arch"
            expected="tfy-bgm $version $deb_arch"
            ;;
        rpm)
            src="$release_dir/bundle/rpm/tfy-bgm-$version-1.$rpm_arch.rpm"
            dest="tfy-bgm-$version-linux-$rpm_arch.rpm"
            [[ -f $src ]] || die "expected RPM not found: $src"
            actual="$(rpm -qp --queryformat '%{NAME} %{VERSION} %{RELEASE} %{ARCH}' "$src")" \
                || die "cannot read RPM metadata: $src"
            expected="tfy-bgm $rpm_version 1 $rpm_arch"
            ;;
    esac
    [[ $actual == "$expected" ]] || die "$fmt metadata mismatch in $src: expected [$expected], found [$actual]"
    sources+=("$src")
    destinations+=("$dest")
done

mkdir -p "$dist"
for i in "${!sources[@]}"; do
    cp -f -- "${sources[i]}" "$dist/${destinations[i]}"
    printf '  %s  <- %s\n' "${destinations[i]}" "${sources[i]}"
done

echo '[3/3] checksums'
artifacts=()
for artifact in "$dist/tfy-bgm-$version"-{windows-{x64,arm64}-{portable.zip,setup.exe},linux-{amd64,arm64}.deb,linux-{x86_64,aarch64}.rpm}; do
    if [[ -f $artifact ]]; then artifacts+=("${artifact##*/}"); fi
done
( cd "$dist" && sha256sum -- "${artifacts[@]}" > SHA256SUMS.txt )
ls -lh -- "${destinations[@]/#/$dist/}"
