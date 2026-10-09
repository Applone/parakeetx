#!/usr/bin/env python3
"""Require every supported Release format and verify each file before publishing."""

import argparse
import hashlib
import pathlib
import tomllib


ROOT = pathlib.Path(__file__).resolve().parents[1]
FORMATS = {
    "x86_64-unknown-linux-gnu": (".tar.gz", ".deb", ".rpm", ".pkg.tar.zst"),
    "x86_64-pc-windows-msvc": (".zip", "-setup.exe"),
    "aarch64-apple-darwin": (".zip", ".dmg"),
}


def verify(directory, version, tag=None):
    if tag is not None and tag.removeprefix("v") != version:
        raise RuntimeError(f"Release tag {tag!r} does not match Cargo version {version!r}")
    expected = {f"parakeetx-{version}-Release-{target}{extension}"
                for target, formats in FORMATS.items() for extension in formats}
    all_files = expected | {name + ".sha256" for name in expected}
    actual = {path.name for path in directory.iterdir() if path.is_file() and path.name != "checksums.txt"}
    if actual != all_files:
        raise RuntimeError(f"Incomplete or unexpected release assets. Missing: {sorted(all_files - actual)}; "
                           f"unexpected: {sorted(actual - all_files)}")
    checksums = []
    for name in sorted(expected):
        artifact = directory / name
        if artifact.stat().st_size == 0:
            raise RuntimeError(f"Empty release asset: {name}")
        with artifact.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        line = f"{digest}  {name}\n"
        if (directory / (name + ".sha256")).read_text(encoding="utf-8") != line:
            raise RuntimeError(f"Checksum mismatch: {name}")
        checksums.append(line)
    return "".join(checksums)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=pathlib.Path, default=ROOT / "artifacts")
    parser.add_argument("--tag")
    parser.add_argument("--tag-only", action="store_true")
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if args.tag_only:
        if args.tag is None or args.tag.removeprefix("v") != version:
            parser.error(f"Expected release tag v{version} or {version}")
        return
    checksums = verify(args.artifacts, version, args.tag)
    (args.artifacts / "checksums.txt").write_text(checksums, encoding="utf-8", newline="\n")
    print(f"Verified all eight release assets for parakeetx {version}")


if __name__ == "__main__":
    main()
