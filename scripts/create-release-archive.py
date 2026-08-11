#!/usr/bin/env python3
"""Create a deterministic .tar.gz archive from one staging directory."""

from __future__ import annotations

import argparse
import gzip
import os
from pathlib import Path
import stat
import tarfile
import tempfile


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="staging directory to archive")
    parser.add_argument("output", type=Path, help="output .tar.gz path")
    parser.add_argument(
        "--mtime",
        required=True,
        type=int,
        help="Unix timestamp applied to every tar entry",
    )
    return parser.parse_args()


def normalized_mode(path: Path) -> int:
    mode = path.lstat().st_mode
    if stat.S_ISDIR(mode):
        return 0o755
    if stat.S_ISLNK(mode):
        return 0o777
    if stat.S_ISREG(mode):
        return 0o755 if mode & stat.S_IXUSR else 0o644
    raise RuntimeError(f"unsupported release archive entry: {path}")


def create_archive(source: Path, output: Path, mtime: int) -> None:
    source = source.resolve(strict=True)
    output = output.resolve()
    if not source.is_dir():
        raise RuntimeError(f"archive source is not a directory: {source}")
    if mtime < 0:
        raise RuntimeError("archive mtime must be non-negative")
    if output == source or source in output.parents:
        raise RuntimeError("archive output must be outside the source directory")

    entries = [source, *sorted(source.rglob("*"), key=lambda path: path.as_posix())]
    output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{output.name}.", suffix=".tmp", dir=output.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as raw:
            with gzip.GzipFile(fileobj=raw, mode="wb", filename="", mtime=0) as compressed:
                with tarfile.open(
                    fileobj=compressed,
                    mode="w",
                    format=tarfile.USTAR_FORMAT,
                ) as archive:
                    for path in entries:
                        archive_name = Path(source.name)
                        if path != source:
                            archive_name /= path.relative_to(source)
                        info = archive.gettarinfo(path, arcname=archive_name.as_posix())
                        info.uid = 0
                        info.gid = 0
                        info.uname = "root"
                        info.gname = "root"
                        info.mtime = mtime
                        info.mode = normalized_mode(path)
                        info.pax_headers = {}
                        if info.isreg():
                            with path.open("rb") as contents:
                                archive.addfile(info, contents)
                        else:
                            archive.addfile(info)
            raw.flush()
            os.fsync(raw.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> int:
    arguments = parse_arguments()
    try:
        create_archive(arguments.source, arguments.output, arguments.mtime)
    except (OSError, RuntimeError, tarfile.TarError) as error:
        print(f"error: {error}", file=os.sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
