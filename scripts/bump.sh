#!/usr/bin/env bash
set -euo pipefail

if [[ $# -gt 1 || ${1:-} == -h || ${1:-} == --help ]]; then
    echo "Usage: $0 [patch|minor|major|X.Y.Z] (default: patch)"
    [[ $# -le 1 ]]
    exit
fi

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
branch=$(git symbolic-ref --quiet --short HEAD) || { echo "Check out a branch first." >&2; exit 1; }
if [[ -n $(git status --porcelain) ]]; then
    echo "Commit or stash all changes before bumping the version." >&2
    exit 1
fi
git remote get-url origin >/dev/null

version=$(python3 - "${1:-patch}" <<'PY'
import pathlib
import plistlib
import re
import subprocess
import sys
import tomllib

manifest = pathlib.Path("Cargo.toml")
lock = pathlib.Path("Cargo.lock")
plist = pathlib.Path("macos/Info.plist")
current = tomllib.loads(manifest.read_text())["package"]["version"]
version_pattern = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
if not re.fullmatch(version_pattern, current):
    sys.exit(f"Expected a stable X.Y.Z version in Cargo.toml, got {current!r}")
parts = list(map(int, current.split(".")))
requested = sys.argv[1]
if requested in ("major", "minor", "patch"):
    index = ("major", "minor", "patch").index(requested)
    parts[index] += 1
    parts[index + 1:] = [0] * (2 - index)
    version = ".".join(map(str, parts))
elif re.fullmatch(version_pattern, requested):
    version = requested
else:
    sys.exit("Use patch, minor, major, or a stable X.Y.Z version.")
if tuple(map(int, version.split("."))) <= tuple(map(int, current.split("."))):
    sys.exit(f"New version must be greater than {current}.")

tag = f"v{version}"
result = subprocess.run(["git", "show-ref", "--verify", "--quiet", f"refs/tags/{tag}"])
if result.returncode == 0:
    sys.exit(f"Tag {tag} already exists.")
if result.returncode != 1:
    sys.exit("Could not check existing tags.")
package = next(p for p in tomllib.loads(lock.read_text())["package"] if p["name"] == "parakeetx")
info = plistlib.loads(plist.read_bytes())
if package["version"] != current or info["CFBundleShortVersionString"] != current:
    sys.exit("Cargo.toml, Cargo.lock, and Info.plist versions must agree.")
if not re.fullmatch(r"[0-9]+", info["CFBundleVersion"]):
    sys.exit("Expected an integer CFBundleVersion in Info.plist.")

def replace_once(text, pattern, value):
    updated, count = re.subn(pattern, lambda m: m[1] + value + m[2], text, flags=re.MULTILINE)
    if count != 1:
        sys.exit(f"Expected exactly one version field matching {pattern!r}.")
    return updated

updates = {
    manifest: replace_once(manifest.read_text(), r'(^\[package\][\s\S]*?^version\s*=\s*")[^"]+(")', version),
    lock: replace_once(lock.read_text(), r'(^name = "parakeetx"\nversion = ")[^"]+(")', version),
    plist: replace_once(plist.read_text(), r'(<key>CFBundleShortVersionString</key>\s*<string>)[^<]+(</string>)', version),
}
updates[plist] = replace_once(updates[plist], r'(<key>CFBundleVersion</key>\s*<string>)[^<]+(</string>)',
                              str(int(info["CFBundleVersion"]) + 1))
for path, content in updates.items():
    path.write_text(content)
print(version)
PY
)

tag="v$version"
git add -- Cargo.toml Cargo.lock macos/Info.plist
git commit -m "chore: bump version to $version"
git tag -a "$tag" -m "Release $tag"
printf '+ git push --atomic origin %q %q\n' "HEAD:refs/heads/$branch" "refs/tags/$tag"
git push --atomic origin "HEAD:refs/heads/$branch" "refs/tags/$tag"
