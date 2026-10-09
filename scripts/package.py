#!/usr/bin/env python3
"""Package a native Cargo build and optionally smoke-test the extracted archive."""

import argparse
import hashlib
import os
import pathlib
import plistlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile

import installers

ROOT = pathlib.Path(__file__).resolve().parents[1]
TARGETS = {
    "x86_64-unknown-linux-gnu": "linux",
    "aarch64-apple-darwin": "macos",
    "x86_64-pc-windows-msvc": "windows",
}


def copy_runtime_libraries(build, destination, platform):
    # ort-sys puts runtime libraries beside the executable. Dereference its
    # symlinks so the archive is independent of the runner's dependency cache.
    patterns = {
        "linux": ("*.so", "*.so.*"),
        "macos": ("*.dylib",),
        "windows": ("*.dll",),
    }[platform]
    for library in sorted({p for pattern in patterns for p in build.glob(pattern)}):
        if library.is_file():
            shutil.copy2(library, destination / library.name)


def copy_resources(destination):
    for name in ("LICENSE", "python/requirements.txt", "docs/ci.md", "docs/macos-audio.md"):
        source = ROOT / name
        if source.is_file():
            target = destination / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
    if runtime := os.environ.get("ORT_LIB_LOCATION"):
        for name in ("LICENSE", "ThirdPartyNotices.txt"):
            source = pathlib.Path(runtime).parent / name
            if source.is_file():
                target = destination / "docs/onnxruntime" / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)


def sign_macos(bundle, executable_directory):
    # Ad-hoc signatures allow local execution on Apple Silicon. Distribution
    # signing and notarization require an Apple Developer identity separately.
    for library in sorted(executable_directory.glob("*.dylib")):
        subprocess.run(["codesign", "--force", "--sign", "-", str(library)], check=True)
    subprocess.run(
        ["codesign", "--force", "--sign", "-", str(executable_directory / "parakeetx")], check=True
    )
    subprocess.run(["codesign", "--force", "--sign", "-", str(bundle)], check=True)
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(bundle)], check=True)


def zip_directory(stage, archive):
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as output:
        for path in sorted(stage.rglob("*")):
            output.write(path, path.relative_to(stage.parent))


def verify_archive(archive, package_name, platform, version):
    with tempfile.TemporaryDirectory(prefix="parakeetx-package-check-") as temporary:
        extracted = pathlib.Path(temporary)
        if archive.name.endswith(".tar.gz"):
            with tarfile.open(archive, "r:gz") as source:
                source.extractall(extracted, filter="data")
        else:
            with zipfile.ZipFile(archive) as source:
                source.extractall(extracted)
                if os.name != "nt":
                    # Python's zip extractor does not restore executable bits.
                    for member in source.infolist():
                        mode = (member.external_attr >> 16) & 0o777
                        if mode:
                            (extracted / member.filename).chmod(mode)
        package = extracted / package_name
        if platform == "macos":
            bundle = package / "parakeetx.app"
            executable = bundle / "Contents/MacOS/parakeetx"
            subprocess.run(["codesign", "--verify", "--deep", "--strict", str(bundle)], check=True)
        else:
            executable = package / ("parakeetx.exe" if platform == "windows" else "parakeetx")
        installers.smoke_executable(executable, version, extracted)


def write_checksum(artifact):
    with artifact.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    artifact.with_name(artifact.name + ".sha256").write_text(
        f"{digest}  {artifact.name}\n", encoding="utf-8", newline="\n"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--configuration", choices=("Debug", "Release"), default="Release")
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--build-dir", type=pathlib.Path,
                        help="Override the Cargo output directory, e.g. target/debug for a host build.")
    parser.add_argument("--output-dir", type=pathlib.Path, default=ROOT / "artifacts")
    parser.add_argument("--verify", action="store_true")
    parser.add_argument("--installers", action="store_true",
                        help="Also create DEB/RPM/Arch packages, an NSIS installer, or a DMG.")
    args = parser.parse_args()
    if args.installers and args.configuration != "Release":
        parser.error("Native installers are only produced from Release builds")
    platform = TARGETS[args.target]
    host = {"linux": "linux", "darwin": "macos", "win32": "windows"}.get(sys.platform)
    if host != platform:
        parser.error("Packaging requires a native build on the target operating system")
    profile = "debug" if args.configuration == "Debug" else "release"
    build = (args.build_dir or ROOT / "target" / args.target / profile).resolve()
    executable = build / ("parakeetx.exe" if platform == "windows" else "parakeetx")
    if not executable.is_file():
        parser.error(f"Build the application first: missing {executable}")
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
    version = manifest["version"]
    if args.installers and not (ROOT / "LICENSE").is_file():
        parser.error("Native packages require the project LICENSE file")
    package_name = f"parakeetx-{version}-{args.configuration}-{args.target}"
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="parakeetx-package-") as temporary:
        stage = pathlib.Path(temporary) / package_name
        stage.mkdir()
        if platform == "macos":
            bundle = stage / "parakeetx.app"
            binary_directory = bundle / "Contents/MacOS"
            resources = bundle / "Contents/Resources"
            binary_directory.mkdir(parents=True)
            resources.mkdir(parents=True)
            with (ROOT / "macos/Info.plist").open("rb") as source:
                info = plistlib.load(source)
            info["CFBundleShortVersionString"] = version
            with (bundle / "Contents/Info.plist").open("wb") as destination:
                plistlib.dump(info, destination)
        else:
            binary_directory = resources = stage
        shutil.copy2(executable, binary_directory / executable.name)
        copy_runtime_libraries(build, binary_directory, platform)
        copy_resources(resources)
        if platform == "macos":
            sign_macos(bundle, binary_directory)
        extension = ".tar.gz" if platform == "linux" else ".zip"
        archive = output / (package_name + extension)
        if platform == "linux":
            with tarfile.open(archive, "w:gz", dereference=True) as destination:
                destination.add(stage, arcname=package_name)
        else:
            zip_directory(stage, archive)
        artifacts = [archive]
        if args.installers:
            if platform == "linux":
                artifacts.extend(installers.linux_packages(stage, output, package_name, version, manifest["license"]))
            elif platform == "windows":
                artifacts.extend(installers.windows_installer(stage, output, package_name, version, args.verify))
            else:
                artifacts.extend(installers.macos_dmg(bundle, output, package_name, version, args.verify))
    if args.verify:
        verify_archive(archive, package_name, platform, version)
    for artifact in artifacts:
        write_checksum(artifact)
        print(artifact)


if __name__ == "__main__":
    main()
