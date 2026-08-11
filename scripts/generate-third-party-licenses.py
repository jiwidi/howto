#!/usr/bin/env python3
"""Generate the deterministic Rust dependency license bundle.

The generator uses Cargo metadata only to locate the exact package sources
selected by Cargo.lock. It fails closed when a locked third-party package has
no packaged license or notice file.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUTPUT = ROOT / "THIRD_PARTY_LICENSES.txt"
LICENSE_NAME = re.compile(
    r"^(?:licen[cs]e|copying|copyright|notice)(?:$|[._-].*)", re.IGNORECASE
)
# A few crates.io packages declare a choice of licenses but accidentally omit
# the corresponding file from the published crate. These are pinned copies of
# an allowed MIT notice from the exact upstream revision (or release tag).
FALLBACK_LICENSES = {
    ("jni", "0.22.4"): "jni-rs-LICENSE-MIT.txt",  # jni-rs/jni-rs@5ae9458a
    ("jni-macros", "0.22.4"): "jni-rs-LICENSE-MIT.txt",  # same workspace/revision
    ("jni-sys-macros", "0.4.1"): "jni-sys-LICENSE-MIT.txt",  # jni-sys@64d77b7a
    (
        "rustls-platform-verifier-android",
        "0.1.1",
    ): "rustls-platform-verifier-LICENSE-MIT.txt",  # tag v/0.1.1
    ("tree-sitter", "0.25.10"): "tree-sitter-LICENSE-MIT.txt",  # @da6fe9be
    (
        "winapi-i686-pc-windows-gnu",
        "0.4.0",
    ): "winapi-LICENSE-MIT.txt",  # winapi-rs 0.4 release line
    (
        "winapi-x86_64-pc-windows-gnu",
        "0.4.0",
    ): "winapi-LICENSE-MIT.txt",  # same release
}


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="generate THIRD_PARTY_LICENSES.txt from Cargo.lock"
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail unless the existing output is current",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT,
        help="output path (default: repository root)",
    )
    return parser.parse_args()


def cargo_metadata() -> dict[str, object]:
    environment = os.environ.copy()
    environment.setdefault("CARGO_TERM_COLOR", "never")
    completed = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version=1",
            "--locked",
            "--offline",
        ],
        cwd=ROOT,
        env=environment,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return json.loads(completed.stdout)


def normalize_text(path: Path) -> str:
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise RuntimeError(f"license file is not UTF-8: {path}") from error
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    return "\n".join(line.rstrip() for line in text.split("\n")).rstrip() + "\n"


def license_files(package: dict[str, object]) -> list[Path]:
    manifest = Path(str(package["manifest_path"]))
    package_root = manifest.parent
    selected: set[Path] = set()

    declared_file = package.get("license_file")
    if declared_file:
        path = Path(str(declared_file)).resolve()
        if not path.is_file():
            raise RuntimeError(
                f"declared license file is missing for {package['name']} "
                f"{package['version']}: {path}"
            )
        selected.add(path)

    for path in package_root.iterdir():
        if path.is_file() and LICENSE_NAME.match(path.name):
            selected.add(path.resolve())

    # r-efi intentionally keeps its complete selectable license notices and
    # copyright list in AUTHORS rather than a LICENSE-named file.
    if not selected and package["name"] == "r-efi":
        selected.add((package_root / "AUTHORS").resolve())

    fallback = FALLBACK_LICENSES.get((str(package["name"]), str(package["version"])))
    if not selected and fallback:
        selected.add((ROOT / "scripts" / "license-fallbacks" / fallback).resolve())

    if not selected:
        raise RuntimeError(
            f"no packaged license or notice file for "
            f"{package['name']} {package['version']}"
        )
    missing = [path for path in selected if not path.is_file()]
    if missing:
        raise RuntimeError(
            f"fallback license file is missing for {package['name']}: {missing[0]}"
        )
    return sorted(selected, key=lambda path: path.name.casefold())


def generate() -> str:
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    metadata = cargo_metadata()
    packages = {
        (item["name"], item["version"], item["source"]): item
        for item in metadata["packages"]
    }
    workspace_ids = set(metadata["workspace_members"])
    workspace_keys = {
        (item["name"], item["version"], item["source"])
        for item in metadata["packages"]
        if item["id"] in workspace_ids
    }
    locked = {
        (item["name"], item["version"], item.get("source")): item
        for item in lock["package"]
        if (item["name"], item["version"], item.get("source")) not in workspace_keys
    }
    missing = sorted(
        set(locked) - set(packages),
        key=lambda item: (item[0].casefold(), item[1], item[2] or ""),
    )
    if missing:
        description = ", ".join(f"{name} {version}" for name, version, _ in missing)
        raise RuntimeError(f"Cargo metadata omitted locked packages: {description}")

    package_records: list[dict[str, object]] = []
    texts: dict[str, str] = {}
    text_users: dict[str, list[str]] = {}

    for key in sorted(
        locked, key=lambda item: (item[0].casefold(), item[1], item[2] or "")
    ):
        package = packages[key]
        lock_entry = locked[key]
        files: list[tuple[str, str]] = []
        for path in license_files(package):
            contents = normalize_text(path)
            digest = hashlib.sha256(contents.encode("utf-8")).hexdigest()
            texts.setdefault(digest, contents)
            user = f"{package['name']} {package['version']} ({path.name})"
            text_users.setdefault(digest, []).append(user)
            files.append((path.name, digest))
        package_records.append(
            {
                "name": package["name"],
                "version": package["version"],
                "source": package["source"] or "local path dependency",
                "checksum": lock_entry.get("checksum", "not recorded"),
                "license": package.get("license") or "not declared",
                "repository": package.get("repository") or "not declared",
                "files": files,
            }
        )

    lines = [
        "HowTo third-party Rust dependency licenses",
        "============================================",
        "",
        "Generated deterministically from Cargo.lock by",
        "scripts/generate-third-party-licenses.py. Do not edit by hand.",
        "",
        f"Locked third-party packages: {len(package_records)}",
        f"Unique included license/notice texts: {len(texts)}",
        "",
        "PACKAGE INDEX",
        "=============",
        "",
    ]
    for record in package_records:
        lines.extend(
            [
                f"{record['name']} {record['version']}",
                f"  Declared license: {record['license']}",
                f"  Source: {record['source']}",
                f"  Cargo checksum: {record['checksum']}",
                f"  Repository: {record['repository']}",
                "  Included files:",
            ]
        )
        for filename, digest in record["files"]:
            lines.append(f"    - {filename} (SHA-256 {digest})")
        lines.append("")

    lines.extend(["INCLUDED LICENSE AND NOTICE TEXTS", "=================================", ""])
    for digest in sorted(texts):
        lines.extend(
            [
                f"SHA-256 {digest}",
                "Used by:",
                *(f"  - {user}" for user in sorted(text_users[digest], key=str.casefold)),
                "",
                texts[digest].rstrip("\n"),
                "",
                "------------------------------------------------------------------------",
                "",
            ]
        )
    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    arguments = parse_arguments()
    try:
        generated = generate()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    output = arguments.output.resolve()
    if arguments.check:
        try:
            existing = output.read_text(encoding="utf-8")
        except FileNotFoundError:
            print(f"error: generated license bundle is missing: {output}", file=sys.stderr)
            return 1
        if existing != generated:
            print(
                "error: THIRD_PARTY_LICENSES.txt is stale; run "
                "scripts/generate-third-party-licenses.py",
                file=sys.stderr,
            )
            return 1
        return 0

    output.write_text(generated, encoding="utf-8", newline="\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
