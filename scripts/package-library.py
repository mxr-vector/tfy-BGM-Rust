#!/usr/bin/env python3
import argparse
import json
import os
from pathlib import Path
import tempfile
import zipfile


REPO = Path(__file__).resolve().parents[1]
AUDIO_EXTENSIONS = {".mp3", ".m4a", ".flac", ".wav", ".ogg"}


def main() -> None:
    parser = argparse.ArgumentParser(description="Package the repository's standalone BGM library.")
    parser.add_argument("--output-dir", type=Path, default=REPO / "dist")
    args = parser.parse_args()
    version = json.loads((REPO / "src-tauri/tauri.conf.json").read_text(encoding="utf-8"))["version"]
    root = REPO / "bgm"
    audio = []
    for directory, dirs, files in os.walk(root):
        current = Path(directory)
        dirs[:] = sorted(d for d in dirs if not d.startswith(".") and not (current / d).is_symlink())
        for name in sorted(files):
            path = current / name
            if not name.startswith(".") and path.suffix.lower() in AUDIO_EXTENSIONS and not path.is_symlink() and path.is_file():
                audio.append(path)
    if not audio:
        parser.error(f"no audio found in {root}")

    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    target = output / f"tfy-bgm-{version}-bgm.zip"
    with tempfile.TemporaryDirectory(dir=output) as temporary:
        archive_path = Path(temporary) / target.name
        with zipfile.ZipFile(archive_path, "w") as archive:
            archive.writestr("bgm/", b"")
            for path in audio:
                compression = zipfile.ZIP_DEFLATED if path.suffix.lower() == ".wav" else zipfile.ZIP_STORED
                archive.write(path, path.relative_to(REPO).as_posix(), compress_type=compression)
        os.replace(archive_path, target)
    print(f"{target}: {len(audio)} audio files, {target.stat().st_size / (1024 * 1024):.1f} MiB")


if __name__ == "__main__":
    main()
