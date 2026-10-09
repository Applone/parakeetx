"""Native package formats for the existing Cargo build and staging directory."""

import os
import pathlib
import re
import shutil
import subprocess
import tempfile


ROOT = pathlib.Path(__file__).resolve().parents[1]


def smoke_executable(executable, version, cwd):
    environment = os.environ.copy()
    for name in ("LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"):
        environment.pop(name, None)
    result = subprocess.run([str(executable), "--version"], cwd=cwd, env=environment,
                            check=True, capture_output=True, text=True, timeout=30)
    if result.stdout.strip() != f"parakeetx {version}":
        raise RuntimeError(f"Unexpected executable version: {result.stdout!r}")
    subprocess.run([str(executable), "--help"], cwd=cwd, env=environment,
                   check=True, capture_output=True, timeout=30)


def required_glibc(symbols):
    # New Rust standard libraries can contain weak imports for newer glibc APIs
    # with a fallback on older systems. Only strong imports set the minimum ABI.
    versions = [tuple(map(int, version.split("."))) for line in symbols.splitlines()
                if " UND " in line and " WEAK " not in line
                for version in re.findall(r"@GLIBC_([0-9.]+)", line)]
    return max(versions, default=(0, 0))


def check_linux_abi(binary_directory):
    for binary in binary_directory.iterdir():
        if binary.name != "parakeetx" and ".so" not in binary.name:
            continue
        symbols = subprocess.check_output(["readelf", "--dyn-syms", "--wide", str(binary)], text=True)
        required = required_glibc(symbols)
        if required > (2, 34):
            version = ".".join(map(str, required))
            raise RuntimeError(f"{binary.name} requires glibc {version}; build Linux installers "
                               "in AlmaLinux 9 for the glibc 2.34 compatibility baseline")


def linux_packages(stage, output, name, version, license_name):
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
        package = output / (name + extension)
        subprocess.run(["nfpm", "package", "--config", str(ROOT / "packaging/nfpm.yml"),
                        "--packager", packager, "--target", str(package)], env=environment, check=True)
        packages.append(package)
    return packages


def windows_installer(stage, output, name, version, verify):
    installer = output / (name + "-setup.exe")
    subprocess.run(["makensis", "/V3", f"/DPACKAGE_STAGE={stage}",
                    f"/DPACKAGE_VERSION={version}", f"/DPACKAGE_OUTPUT={installer}",
                    str(ROOT / "packaging/windows/installer.nsi")], check=True)
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
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key, 0,
                                    winreg.KEY_READ | winreg.KEY_WOW64_64KEY) as registry:
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
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key, 0,
                                winreg.KEY_READ | winreg.KEY_WOW64_64KEY):
                raise RuntimeError("Uninstaller left its registry entry behind")
        except FileNotFoundError:
            pass


def macos_dmg(bundle, output, name, version, verify):
    image = output / (name + ".dmg")
    with tempfile.TemporaryDirectory(prefix="parakeetx-dmg-") as temporary:
        source = pathlib.Path(temporary) / "volume"
        source.mkdir()
        shutil.copytree(bundle, source / bundle.name)
        (source / "Applications").symlink_to("/Applications", target_is_directory=True)
        subprocess.run(["hdiutil", "create", "-ov", "-volname", "parakeetx", "-fs", "HFS+",
                        "-format", "UDZO", "-srcfolder", str(source), str(image)], check=True)
    if verify:
        verify_macos_dmg(image, version)
    return [image]


def verify_macos_dmg(image, version):
    subprocess.run(["hdiutil", "verify", str(image)], check=True)
    with tempfile.TemporaryDirectory(prefix="parakeetx-dmg-check-") as temporary:
        root = pathlib.Path(temporary)
        mount = root / "mounted"
        mount.mkdir()
        subprocess.run(["hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", str(mount),
                        str(image)], check=True)
        try:
            if os.readlink(mount / "Applications") != "/Applications":
                raise RuntimeError("DMG is missing its Applications shortcut")
            installed = root / "Applications/parakeetx.app"
            shutil.copytree(mount / "parakeetx.app", installed)
            subprocess.run(["codesign", "--verify", "--deep", "--strict", str(installed)], check=True)
            smoke_executable(installed / "Contents/MacOS/parakeetx", version, root)
        finally:
            subprocess.run(["hdiutil", "detach", str(mount)], check=True)
