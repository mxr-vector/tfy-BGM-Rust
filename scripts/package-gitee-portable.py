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
import struct
import tempfile
import zipfile


REPO = Path(__file__).resolve().parents[1]

# 与 scripts/package-common.ps1 的 Get-BundledSeparationModels 保持同步。
BUNDLED_SEPARATION_MODELS = (
    "UVR-MDX-NET-Inst_HQ_3.onnx",
    "UVR-MDX-NET-Voc_FT.onnx",
    "sherpa-onnx-spleeter-2stems-int8.tar.bz2",
)


def app_version() -> str:
    conf = json.loads((REPO / "src-tauri/tauri.conf.json").read_text(encoding="utf-8"))
    version = conf["version"]
    cargo_toml = (REPO / "src-tauri/Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml, re.MULTILINE)
    if not match or match.group(1) != version:
        raise RuntimeError("tauri.conf.json 与 Cargo.toml 的版本不一致")
    return version


def verify_windows_x64(exe: Path) -> None:
    with exe.open("rb") as stream:
        dos_header = stream.read(64)
        if len(dos_header) != 64 or dos_header[:2] != b"MZ":
            raise ValueError(f"不是 Windows PE 文件（缺少有效的 DOS 头）：{exe}")

        pe_offset = struct.unpack_from("<I", dos_header, 0x3C)[0]
        # PE 签名 (4)、COFF 文件头 (20) 与可选头魔数 (2)。
        if not 64 <= pe_offset <= os.fstat(stream.fileno()).st_size - 26:
            raise ValueError(f"无效的 Windows PE 头偏移：{exe}")
        stream.seek(pe_offset)
        pe_header = stream.read(26)
        if len(pe_header) != 26 or pe_header[:4] != b"PE\0\0":
            raise ValueError(f"无效的 Windows PE 签名或文件头：{exe}")

        machine = struct.unpack_from("<H", pe_header, 4)[0]
        optional_size, flags, magic = struct.unpack_from("<HHH", pe_header, 20)
        # AMD64、PE32+，并且是可执行映像而非 DLL。
        if (machine, magic) != (0x8664, 0x20B) or not flags & 0x0002 or flags & 0x2000:
            raise ValueError(f"不是 Windows x64 PE 可执行文件：{exe}")
        if optional_size < 112 or len(stream.read(optional_size - 2)) != optional_size - 2:
            raise ValueError(f"无效或截断的 Windows PE 可选头：{exe}")


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
        required=True,
        help="交叉编译得到的 tfy-bgm.exe",
    )
    parser.add_argument("--output-dir", type=Path, default=REPO / "dist")
    parser.add_argument("--max-mib", type=int, default=100, help="Gitee 单附件上限")
    parser.add_argument(
        "--with-models",
        action="store_true",
        help="附带人声分离模型（约 174 MiB）。Gitee 单附件 100 MiB 限制下必须同时抬高 --max-mib 或改用分卷",
    )
    args = parser.parse_args()

    version = app_version()
    exe = args.exe.resolve()
    if not exe.is_file():
        parser.error(f"找不到 Windows 可执行文件：{exe}")
    try:
        verify_windows_x64(exe)
    except ValueError as error:
        parser.error(str(error))

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
        # 逐字稿 prompts.json 与音频同目录，缺了它内置音色无法用于克隆。
        for asset in sorted((REPO / "models/voices").iterdir()):
            if asset.is_file():
                shutil.copy2(asset, voices / asset.name)

        # 运行时按 Resource/models 解析分离模型（Windows 上即 exe 同级），
        # 与 scripts/package-common.ps1 的 Copy-BundledSeparationModels 保持一致。
        if args.with_models:
            models = package / "models"
            models.mkdir()
            for name in BUNDLED_SEPARATION_MODELS:
                source = REPO / "models" / name
                if not source.is_file():
                    raise FileNotFoundError(f"缺少随包分离模型：{source}")
                shutil.copy2(source, models / name)

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
        size_bytes = temporary_zip.stat().st_size
        size_mib = size_bytes / (1024 * 1024)
        if size_bytes > args.max_mib * 1024 * 1024:
            raise RuntimeError(
                f"{zip_path.name} 为 {size_mib:.1f} MiB，超过 Gitee 单附件 {args.max_mib} MiB 限制"
            )
        os.replace(temporary_zip, zip_path)

    checksum_path = write_checksums(output_dir, version)
    print(f"Archive: {zip_path} ({size_mib:.1f} MiB)")
    print(f"Checksums: {checksum_path}")


if __name__ == "__main__":
    main()
