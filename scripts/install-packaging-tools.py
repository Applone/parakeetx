#!/usr/bin/env python3
"""Install the pinned nFPM binary after verifying its release checksum."""

import hashlib
import json
import os
import pathlib
import subprocess
import tarfile
import tempfile
import urllib.request


ROOT = pathlib.Path(__file__).resolve().parents[1]


def main():
    version = json.loads((ROOT / "packaging/tools.json").read_text())["nfpm"]
    filename = f"nfpm_{version}_Linux_x86_64.tar.gz"
    base = f"https://github.com/goreleaser/nfpm/releases/download/v{version}"
    destination = pathlib.Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir())) / "parakeetx-tools"
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="parakeetx-nfpm-") as temporary:
        archive = pathlib.Path(temporary) / filename
        with urllib.request.urlopen(f"{base}/checksums.txt", timeout=60) as response:
            checksums = response.read().decode("utf-8")
        matches = [fields[0] for line in checksums.splitlines()
                   if len(fields := line.split()) == 2 and fields[1].lstrip("*") == filename]
        if len(matches) != 1:
            raise RuntimeError("nFPM release does not have one checksum for the selected binary")
        with urllib.request.urlopen(f"{base}/{filename}", timeout=120) as response, archive.open("wb") as output:
            while chunk := response.read(1024 * 1024):
                output.write(chunk)
        with archive.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        if digest != matches[0]:
            raise RuntimeError("nFPM download checksum mismatch")
        with tarfile.open(archive, "r:gz") as source:
            member = source.getmember("nfpm")
            if not member.isfile():
                raise RuntimeError("nFPM archive does not contain a regular executable")
            with source.extractfile(member) as binary, (destination / "nfpm").open("wb") as output:
                output.write(binary.read())
        (destination / "nfpm").chmod(0o755)
    subprocess.run([str(destination / "nfpm"), "--version"], check=True)
    if path_file := os.environ.get("GITHUB_PATH"):
        with open(path_file, "a", encoding="utf-8") as output:
            output.write(f"{destination}\n")
    else:
        print(f"Add {destination} to PATH before packaging")


if __name__ == "__main__":
    main()
