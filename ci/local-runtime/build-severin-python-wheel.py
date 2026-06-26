#!/usr/bin/env python3
"""Assemble the visible Severin Python launcher/controller wheel.

The visible Python package is pure Python and imports as ``severin``. It launches
and controls the single headed ``severin`` executable through inherited anonymous
pipe FDs. The older in-process CPython extension is intentionally not packaged by
this script, so it cannot shadow the visible launcher package.
"""

from __future__ import annotations

import argparse
import base64
import csv
import hashlib
import os
import re
import shutil
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile


def workspace_version(repo: Path) -> str:
    cargo_toml = repo / "Cargo.toml"
    text = cargo_toml.read_text(encoding="utf-8")
    in_workspace_package = False
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if line.startswith("[") and line.endswith("]"):
            in_workspace_package = line == "[workspace.package]"
            continue
        if in_workspace_package:
            match = re.match(r'version\s*=\s*"([^"]+)"', line)
            if match:
                return match.group(1)
    raise SystemExit("Could not find [workspace.package] version in Cargo.toml")


def wheel_hash(data: bytes) -> str:
    digest = hashlib.sha256(data).digest()
    return "sha256=" + base64.urlsafe_b64encode(digest).rstrip(b"=").decode("ascii")


def write_wheel_record(wheel_path: Path) -> None:
    rows: list[list[str]] = []
    with ZipFile(wheel_path, "r") as zf:
        record_name = next(name for name in zf.namelist() if name.endswith(".dist-info/RECORD"))
        for name in zf.namelist():
            if name == record_name:
                rows.append([name, "", ""])
            else:
                data = zf.read(name)
                rows.append([name, wheel_hash(data), str(len(data))])

    rendered = []
    for row in rows:
        from io import StringIO

        buf = StringIO()
        csv.writer(buf, lineterminator="\n").writerow(row)
        rendered.append(buf.getvalue())

    tmp_path = wheel_path.with_suffix(".tmp.whl")
    with ZipFile(wheel_path, "r") as zin, ZipFile(
        tmp_path, "w", compression=ZIP_DEFLATED, compresslevel=9, strict_timestamps=False
    ) as zout:
        for item in zin.infolist():
            if item.filename == record_name:
                continue
            zout.writestr(item, zin.read(item.filename))
        zout.writestr(record_name, "".join(rendered))
    tmp_path.replace(wheel_path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default=os.environ.get("GITHUB_WORKSPACE", "."))
    parser.add_argument("--output-dir", default="release")
    # Kept so older workflow invocations do not accidentally re-enable extension packaging.
    parser.add_argument("--target-dir", default=os.environ.get("CARGO_TARGET_DIR", "target"))
    parser.add_argument("--profile", default="release")
    parser.add_argument("--skip-cargo-build", action="store_true")
    args = parser.parse_args()

    repo = Path(args.repo).resolve()
    output_dir = (repo / args.output_dir).resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    version = workspace_version(repo)
    wheel_name = f"severin-{version}-py3-none-any.whl"
    dist_info = f"severin-{version}.dist-info"
    wheel_path = output_dir / wheel_name

    package_source = repo / "ports" / "severin-python" / "severin"
    if not (package_source / "__init__.py").exists():
        raise SystemExit(f"Launcher package not found: {package_source}")

    metadata = (
        "Metadata-Version: 2.1\n"
        "Name: severin\n"
        f"Version: {version}\n"
        "Summary: Pure-Python launcher/controller for the headed Severin runtime.\n"
        "License: MPL-2.0\n"
        "Requires-Python: >=3.11\n"
    )
    wheel = (
        "Wheel-Version: 1.0\n"
        "Generator: ci/local-runtime/build-severin-python-wheel.py\n"
        "Root-Is-Purelib: true\n"
        "Tag: py3-none-any\n"
    )

    staging = output_dir / ".severin-wheel-root"
    if staging.exists():
        shutil.rmtree(staging)
    shutil.copytree(package_source, staging / "severin")

    with ZipFile(
        wheel_path, "w", compression=ZIP_DEFLATED, compresslevel=9, strict_timestamps=False
    ) as zf:
        for path in sorted((staging / "severin").rglob("*")):
            if path.is_file():
                zf.write(path, path.relative_to(staging).as_posix())
        zf.writestr(f"{dist_info}/METADATA", metadata)
        zf.writestr(f"{dist_info}/WHEEL", wheel)
        zf.writestr(f"{dist_info}/RECORD", "")
    shutil.rmtree(staging)
    write_wheel_record(wheel_path)

    print(f"wheel={wheel_path}")
    print(f"wheel_name={wheel_name}")
    print("python_tag=py3")
    print("abi_tag=none")
    print("platform_tag=any")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
