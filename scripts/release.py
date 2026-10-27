#!/usr/bin/env python3
"""Package native builds, install packaging tools, and verify release assets."""

import argparse
import hashlib
import json
import os
import pathlib
import plistlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]

TARGETS = {
    "x86_64-unknown-linux-gnu": "linux",
    "aarch64-apple-darwin": "macos",
    "x86_64-pc-windows-msvc": "windows",
}

FORMATS = {
    "linux": {
        ".tar.gz": "linux-portable-x64",
        ".deb": "linux-deb-x64",
        ".rpm": "linux-rpm-x64",
        ".pkg.tar.zst": "linux-arch-x64",
    },
    "windows": {
        ".zip": "windows-portable-x64",
        ".exe": "windows-setup-x64",
    },
    "macos": {
        ".zip": "macos-portable-arm64",
        ".dmg": "macos-installer-arm64",
    },
}


def artifact_name(platform, version, extension, configuration="Release"):
    suffix = {"Release": "", "Debug": "-debug"}[configuration]
    return f"parakeetx-{FORMATS[platform][extension]}-{version}{suffix}{extension}"


def smoke_executable(executable, version, cwd):
    environment = os.environ.copy()
    for name in ("LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"):
        environment.pop(name, None)
    result = subprocess.run(
        [str(executable), "--version"], cwd=cwd, env=environment, check=True, capture_output=True, text=True, timeout=30
    )
    if result.stdout.strip() != f"parakeetx {version}":
        raise RuntimeError(f"Unexpected executable version: {result.stdout!r}")
    subprocess.run([str(executable), "--help"], cwd=cwd, env=environment, check=True, capture_output=True, timeout=30)


def required_glibc(symbols):
    # New Rust standard libraries can contain weak imports for newer glibc APIs
    # with a fallback on older systems. Only strong imports set the minimum ABI.
    versions = [
        tuple(map(int, version.split(".")))
        for line in symbols.splitlines()
        if " UND " in line and " WEAK " not in line
        for version in re.findall(r"@GLIBC_([0-9.]+)", line)
    ]
    return max(versions, default=(0, 0))


def check_linux_abi(binary_directory):
    for binary in binary_directory.iterdir():
        if binary.name != "parakeetx" and ".so" not in binary.name:
            continue
        symbols = subprocess.check_output(["readelf", "--dyn-syms", "--wide", str(binary)], text=True)
        required = required_glibc(symbols)
        if required > (2, 34):
            version = ".".join(map(str, required))
            raise RuntimeError(
                f"{binary.name} requires glibc {version}; build Linux installers "
                "in AlmaLinux 9 for the glibc 2.34 compatibility baseline"
            )


def linux_packages(stage, output, version, license_name):
    if license_name != "GPL-3.0-only":
        raise RuntimeError("Update packaging/nfpm.yml to match the Cargo manifest license")
    check_linux_abi(stage)
    filesystem = stage.parent / "linux-root"
    binaries = filesystem / "usr/lib/parakeetx"
    binaries.mkdir(parents=True)
    shutil.copy2(stage / "parakeetx", binaries / "parakeetx")
    for library in stage.glob("*.so*"):
        shutil.copy2(library, binaries / library.name)
    documentation = filesystem / "usr/share/doc/parakeetx"
    documentation.mkdir(parents=True)
    if (stage / "docs").is_dir():
        shutil.copytree(stage / "docs", documentation, dirs_exist_ok=True)
    shutil.copy2(stage / "LICENSE", documentation / "LICENSE")
    shutil.copytree(stage / "python", filesystem / "usr/share/parakeetx/python")
    applications = filesystem / "usr/share/applications"
    applications.mkdir(parents=True)
    desktop = ROOT / "packaging/linux/app.parakeetx.desktop.desktop"
    subprocess.run(["desktop-file-validate", str(desktop)], check=True)
    shutil.copy2(desktop, applications / desktop.name)
    environment = dict(os.environ, PACKAGE_ROOT=str(filesystem), PACKAGE_VERSION=version)
    packages = []
    for packager, extension in (("deb", ".deb"), ("rpm", ".rpm"), ("archlinux", ".pkg.tar.zst")):
        package = output / artifact_name("linux", version, extension)
        subprocess.run(
            [
                "nfpm",
                "package",
                "--config",
                str(ROOT / "packaging/nfpm.yml"),
                "--packager",
                packager,
                "--target",
                str(package),
            ],
            env=environment,
            check=True,
        )
        packages.append(package)
    return packages


def windows_installer(stage, output, version, verify):
    installer = output / artifact_name("windows", version, ".exe")
    subprocess.run(
        [
            "makensis",
            "/V3",
            f"/DPACKAGE_STAGE={stage}",
            f"/DPACKAGE_VERSION={version}",
            f"/DPACKAGE_OUTPUT={installer}",
            str(ROOT / "packaging/windows/installer.nsi"),
        ],
        check=True,
    )
    if verify:
        verify_windows_installer(installer, version)
    return [installer]


def verify_windows_installer(installer, version):
    import winreg

    key = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\parakeetx"
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key, 0, winreg.KEY_READ | winreg.KEY_WOW64_64KEY):
            raise RuntimeError("Installer verification requires a user without an existing parakeetx installation")
    except FileNotFoundError:
        pass
    with tempfile.TemporaryDirectory(prefix="parakeetx-installer-check-") as temporary:
        root = pathlib.Path(temporary)
        install = root / "application"
        try:
            # NSIS requires /D last, without quotes even when the path has spaces.
            # A string is passed directly to CreateProcess, without a shell.
            for _ in range(2):
                subprocess.run(f'"{installer}" /S /D={install}', check=True, timeout=120)
                smoke_executable(install / "parakeetx.exe", version, root)
                with winreg.OpenKey(
                    winreg.HKEY_CURRENT_USER, key, 0, winreg.KEY_READ | winreg.KEY_WOW64_64KEY
                ) as registry:
                    if winreg.QueryValueEx(registry, "DisplayVersion")[0] != version:
                        raise RuntimeError("Installed version was not registered correctly")
                    if pathlib.Path(winreg.QueryValueEx(registry, "InstallLocation")[0]) != install:
                        raise RuntimeError("Installer did not use the requested installation directory")
        finally:
            uninstaller = install / "Uninstall.exe"
            if uninstaller.exists():
                # _?= runs the uninstaller in place so the process wait includes
                # removal, rather than waiting only for its temporary launcher.
                subprocess.run(f'"{uninstaller}" /S _?={install}', check=True, timeout=120)
                uninstaller.unlink(missing_ok=True)
        if (install / "parakeetx.exe").exists():
            raise RuntimeError("Uninstaller left the application executable behind")
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key, 0, winreg.KEY_READ | winreg.KEY_WOW64_64KEY):
                raise RuntimeError("Uninstaller left its registry entry behind")
        except FileNotFoundError:
            pass


def macos_dmg(bundle, output, version, verify):
    image = output / artifact_name("macos", version, ".dmg")
    with tempfile.TemporaryDirectory(prefix="parakeetx-dmg-") as temporary:
        source = pathlib.Path(temporary) / "volume"
        source.mkdir()
        shutil.copytree(bundle, source / bundle.name)
        (source / "Applications").symlink_to("/Applications", target_is_directory=True)
        subprocess.run(
            [
                "hdiutil",
                "create",
                "-ov",
                "-volname",
                "parakeetx",
                "-fs",
                "HFS+",
                "-format",
                "UDZO",
                "-srcfolder",
                str(source),
                str(image),
            ],
            check=True,
        )
    if verify:
        verify_macos_dmg(image, version)
    return [image]


def verify_macos_dmg(image, version):
    subprocess.run(["hdiutil", "verify", str(image)], check=True)
    with tempfile.TemporaryDirectory(prefix="parakeetx-dmg-check-") as temporary:
        root = pathlib.Path(temporary)
        mount = root / "mounted"
        mount.mkdir()
        subprocess.run(
            ["hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", str(mount), str(image)], check=True
        )
        try:
            if os.readlink(mount / "Applications") != "/Applications":
                raise RuntimeError("DMG is missing its Applications shortcut")
            installed = root / "Applications/parakeetx.app"
            shutil.copytree(mount / "parakeetx.app", installed)
            subprocess.run(["codesign", "--verify", "--deep", "--strict", str(installed)], check=True)
            smoke_executable(installed / "Contents/MacOS/parakeetx", version, root)
        finally:
            subprocess.run(["hdiutil", "detach", str(mount)], check=True)


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
    for name in ("LICENSE", "python/requirements.txt"):
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
    subprocess.run(["codesign", "--force", "--sign", "-", str(executable_directory / "parakeetx")], check=True)
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
        smoke_executable(executable, version, extracted)


def write_checksum(artifact):
    with artifact.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    artifact.with_name(artifact.name + ".sha256").write_text(
        f"{digest}  {artifact.name}\n", encoding="utf-8", newline="\n"
    )


def create_package(args, parser):
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
    extension = ".tar.gz" if platform == "linux" else ".zip"
    archive_name = artifact_name(platform, version, extension, args.configuration)
    package_name = archive_name.removesuffix(extension)
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
        archive = output / archive_name
        if platform == "linux":
            with tarfile.open(archive, "w:gz", dereference=True) as destination:
                destination.add(stage, arcname=package_name)
        else:
            zip_directory(stage, archive)
        artifacts = [archive]
        if args.installers:
            if platform == "linux":
                artifacts.extend(linux_packages(stage, output, version, manifest["license"]))
            elif platform == "windows":
                artifacts.extend(windows_installer(stage, output, version, args.verify))
            else:
                artifacts.extend(macos_dmg(bundle, output, version, args.verify))
    if args.verify:
        verify_archive(archive, package_name, platform, version)
    for artifact in artifacts:
        write_checksum(artifact)
        print(artifact)


def install_tools():
    version = json.loads((ROOT / "packaging/tools.json").read_text())["nfpm"]
    filename = f"nfpm_{version}_Linux_x86_64.tar.gz"
    base = f"https://github.com/goreleaser/nfpm/releases/download/v{version}"
    destination = pathlib.Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir())) / "parakeetx-tools"
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="parakeetx-nfpm-") as temporary:
        archive = pathlib.Path(temporary) / filename
        with urllib.request.urlopen(f"{base}/checksums.txt", timeout=60) as response:
            checksums = response.read().decode("utf-8")
        matches = [
            fields[0]
            for line in checksums.splitlines()
            if len(fields := line.split()) == 2 and fields[1].lstrip("*") == filename
        ]
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


def verify_release(directory, version, tag=None):
    if tag is not None and tag.removeprefix("v") != version:
        raise RuntimeError(f"Release tag {tag!r} does not match Cargo version {version!r}")
    expected = {
        artifact_name(platform, version, extension) for platform, formats in FORMATS.items() for extension in formats
    }
    all_files = expected | {name + ".sha256" for name in expected}
    actual = {path.name for path in directory.iterdir() if path.is_file() and path.name != "checksums.txt"}
    if actual != all_files:
        raise RuntimeError(
            f"Incomplete or unexpected release assets. Missing: {sorted(all_files - actual)}; "
            f"unexpected: {sorted(actual - all_files)}"
        )
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
    commands = parser.add_subparsers(dest="command", required=True)
    package = commands.add_parser("package", help="Package an existing native Cargo build.")
    package.add_argument("--configuration", choices=("Debug", "Release"), default="Release")
    package.add_argument("--target", choices=TARGETS, required=True)
    package.add_argument(
        "--build-dir",
        type=pathlib.Path,
        help="Override the Cargo output directory, e.g. target/debug for a host build.",
    )
    package.add_argument("--output-dir", type=pathlib.Path, default=ROOT / "artifacts")
    package.add_argument("--verify", action="store_true")
    package.add_argument(
        "--installers", action="store_true", help="Also create DEB/RPM/Arch packages, an NSIS installer, or a DMG."
    )
    commands.add_parser("install-tools", help="Install the pinned nFPM binary with checksum verification.")
    verify = commands.add_parser("verify", help="Check release assets and write checksums.txt, or validate a tag.")
    verify.add_argument("--artifacts", type=pathlib.Path, default=ROOT / "artifacts")
    verify.add_argument("--tag")
    verify.add_argument("--tag-only", action="store_true")
    args = parser.parse_args()
    if args.command == "package":
        create_package(args, package)
    elif args.command == "install-tools":
        install_tools()
    else:
        version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
        if args.tag_only:
            if args.tag is None or args.tag.removeprefix("v") != version:
                verify.error(f"Expected release tag v{version} or {version}")
            return
        checksums = verify_release(args.artifacts, version, args.tag)
        (args.artifacts / "checksums.txt").write_text(checksums, encoding="utf-8", newline="\n")
        print(f"Verified all eight release assets for parakeetx {version}")


if __name__ == "__main__":
    main()
