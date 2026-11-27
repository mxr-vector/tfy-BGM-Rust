#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
    printf 'macOS 打包需要 macOS 和 Xcode 26 或更新版本。\n' >&2
    exit 1
fi

profile=release
cargo_flags=(--locked)
for argument in "$@"; do
    case "$argument" in
        --debug) profile=debug ;;
        --offline) cargo_flags+=(--offline) ;;
        --runtime-shaders) cargo_flags+=(--features gpui/runtime_shaders) ;;
        *)
            printf '用法 %s [--debug] [--offline] [--runtime-shaders]\n' "$0" >&2
            exit 2
            ;;
    esac
done
if [[ "$profile" == release ]]; then
    cargo_flags+=(--release)
fi

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"
runtime_dir="${TFY_ASR_RUNTIME_DIR:-}"
if [[ -n "$runtime_dir" && ! -s "$runtime_dir/libsherpa-onnx-c-api.dylib" ]]; then
    printf 'TFY_ASR_RUNTIME_DIR 缺少 libsherpa-onnx-c-api.dylib：%s\n' "$runtime_dir" >&2
    exit 1
fi
target="$repo/target"
mkdir -p "$target/macos"
staging="$(mktemp -d "$target/macos/staging.XXXXXX")"
trap 'rm -rf "$staging"' EXIT
app="$staging/TFY-BGM.app"
resources="$app/Contents/Resources"
mkdir -p "$app/Contents/MacOS" "$resources"

plist="$app/Contents/Info.plist"
install -m 644 "$repo/assets/macos/Info.plist" "$plist"
minimum="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$plist")"

xcrun --sdk macosx actool "$repo/assets/macos/AppIcon.icon" \
    --compile "$resources" --app-icon AppIcon --platform macosx --target-device mac \
    --minimum-deployment-target "$minimum" --output-partial-info-plist "$staging/icon.plist" \
    --errors --warnings --output-format human-readable-text
test -s "$resources/Assets.car"
/usr/libexec/PlistBuddy -c "Merge \"$staging/icon.plist\"" "$plist"

cargo build --target-dir "$target" "${cargo_flags[@]}"
install -m 755 "$target/$profile/tfy-bgm-gpui" "$app/Contents/MacOS/tfy-bgm-gpui"
if [[ -d "$repo/assets/bg" ]]; then
    mkdir -p "$resources/assets/bg"
    for bg in "$repo/assets/bg"/*; do
        [[ -f "$bg" ]] || continue
        name="${bg##*/}"
        [[ "$name" == .* || "$name" == *.thumb.* ]] && continue
        install -m 644 "$bg" "$resources/assets/bg/$name"
    done
fi
if [[ -d "$repo/models/voices" ]]; then
    mkdir -p "$resources/voices"
    for voice in "$repo/models/voices"/*; do
        [[ -f "$voice" ]] || continue
        install -m 644 "$voice" "$resources/voices/${voice##*/}"
    done
fi
models="${TFY_SEPARATION_MODELS_DIR:-$repo/models}"
for model in UVR-MDX-NET-Inst_HQ_3.onnx UVR-MDX-NET-Voc_FT.onnx sherpa-onnx-spleeter-2stems-int8.tar.bz2; do
    if [[ -s "$models/$model" ]]; then
        mkdir -p "$resources/models"
        install -m 644 "$models/$model" "$resources/models/$model"
    fi
done
if [[ -n "$runtime_dir" ]]; then
    mkdir -p "$resources/asr"
    for library in "$runtime_dir"/*.dylib; do
        [[ -f "$library" ]] || continue
        embedded="$resources/asr/${library##*/}"
        install -m 755 "$library" "$embedded"
        codesign --force --sign - "$embedded"
    done
fi
version="$(sed -nE 's/^version = "([0-9]+\.[0-9]+\.[0-9]+)[^"]*"$/\1/p' Cargo.toml)"
/usr/libexec/PlistBuddy -c "Add :CFBundleShortVersionString string $version" "$plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleVersion string $version" "$plist"
plutil -lint "$plist"
codesign --force --sign - "$app"
codesign --verify --strict "$app"

output="$target/macos/TFY-BGM.app"
if [[ -e "$output" ]]; then
    mv "$output" "$staging/previous.app"
fi
if ! mv "$app" "$output"; then
    if [[ -e "$staging/previous.app" ]]; then
        mv "$staging/previous.app" "$output"
    fi
    exit 1
fi
printf '%s\n' "$output"
