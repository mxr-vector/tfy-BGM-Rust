#!/usr/bin/env python3
"""把 Linux 上交叉编译的 Windows 可执行文件打包成 Gitee 绿色版。"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import tempfile
import zipfile


REPO = Path(__file__).resolve().parents[1]


def app_version() -> str:
    conf = json.loads((REPO / "src-tauri/tauri.conf.json").read_text(encoding="utf-8"))
    version = conf["version"]
    cargo_toml = (REPO / "src-tauri/Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml, re.MULTILINE)
    if not match or match.group(1) != version:
        raise RuntimeError("tauri.conf.json 与 Cargo.toml 的版本不一致")
    return version


def write_checksums(output_dir: Path, version: str) -> Path:
    artifacts = sorted(output_dir.glob(f"tfy-bgm-{version}-*"))
    if not artifacts:
        raise RuntimeError("没有可生成校验值的当前版本产物")

    checksum_path = output_dir / "SHA256SUMS.txt"
    lines = []
    for artifact in artifacts:
        digest = hashlib.sha256()
        with artifact.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        lines.append(f"{digest.hexdigest()}  {artifact.name}\n")

    temporary = output_dir / ".SHA256SUMS.txt.tmp"
    temporary.write_text("".join(lines), encoding="utf-8", newline="\n")
    os.replace(temporary, checksum_path)
    return checksum_path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--exe",
        type=Path,
        default=REPO
        / "src-tauri/target/x86_64-pc-windows-msvc/release/tfy-bgm.exe",
        help="交叉编译得到的 tfy-bgm.exe",
    )
    parser.add_argument("--output-dir", type=Path, default=REPO / "dist")
    parser.add_argument("--max-mib", type=int, default=100, help="Gitee 单附件上限")
    args = parser.parse_args()

    version = app_version()
    exe = args.exe.resolve()
    if not exe.is_file():
        raise FileNotFoundError(f"找不到 Windows 可执行文件：{exe}")

    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    zip_path = output_dir / f"tfy-bgm-{version}-windows-x64-portable.zip"

    with tempfile.TemporaryDirectory(prefix=".tfy-bgm-", dir=output_dir) as temporary:
        package = Path(temporary) / "tfy-bgm"
        package.mkdir()
        shutil.copy2(exe, package / "tfy-bgm.exe")
        shutil.copy2(REPO / "LICENSE", package / "LICENSE")
        shutil.copy2(REPO / "README.md", package / "README.md")
        shutil.copytree(REPO / "assets/bg", package / "assets/bg")

        voices = package / "voices"
        voices.mkdir()
        for prompt in sorted((REPO / "models/voices").glob("*.wav")):
            shutil.copy2(prompt, voices / prompt.name)

        categories = json.loads(
            (REPO / "src-tauri/core/assets/categories.default.json").read_text(
                encoding="utf-8"
            )
        )
        for category in categories:
            (package / "bgm" / category["id"]).mkdir(parents=True, exist_ok=True)

        temporary_zip = Path(temporary) / zip_path.name
        with zipfile.ZipFile(
            temporary_zip, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6
        ) as archive:
            for path in sorted(package.rglob("*")):
                relative = path.relative_to(package).as_posix()
                archive_path = f"tfy-bgm/{relative}"
                if path.is_dir():
                    archive.writestr(f"{archive_path}/", b"")
                else:
                    archive.write(path, archive_path)
        os.replace(temporary_zip, zip_path)

    size_mib = zip_path.stat().st_size / (1024 * 1024)
    if size_mib > args.max_mib:
        raise RuntimeError(
            f"{zip_path.name} 为 {size_mib:.1f} MiB，超过 Gitee 单附件 {args.max_mib} MiB 限制"
        )

    checksum_path = write_checksums(output_dir, version)
    print(f"Archive: {zip_path} ({size_mib:.1f} MiB)")
    print(f"Checksums: {checksum_path}")


if __name__ == "__main__":
    main()
