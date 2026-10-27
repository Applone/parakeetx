import hashlib
import os
import pathlib
import tempfile
import unittest

import release


class ReleaseTests(unittest.TestCase):
    @unittest.skipIf(os.name == "nt", "POSIX executable permissions require a Unix host")
    def test_zip_extraction_restores_executable_permissions(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            stage = directory / "parakeetx-test"
            stage.mkdir()
            executable = stage / "parakeetx"
            executable.write_text("#!/bin/sh\nprintf 'parakeetx 0.1.0\\n'\n", encoding="utf-8")
            executable.chmod(0o755)
            archive = directory / "application.zip"
            release.zip_directory(stage, archive)
            release.verify_archive(archive, stage.name, "linux", "0.1.0")

    def populate(self, directory):
        for platform, formats in release.FORMATS.items():
            for extension in formats:
                path = directory / release.artifact_name(platform, "0.1.0", extension)
                path.write_bytes(b"a nonempty package fixture")
                release.write_checksum(path)

    def test_release_names_identify_platform_package_and_architecture(self):
        expected = {
            "parakeetx-linux-portable-x64-1.1.0.tar.gz",
            "parakeetx-linux-deb-x64-1.1.0.deb",
            "parakeetx-linux-rpm-x64-1.1.0.rpm",
            "parakeetx-linux-arch-x64-1.1.0.pkg.tar.zst",
            "parakeetx-windows-portable-x64-1.1.0.zip",
            "parakeetx-windows-setup-x64-1.1.0.exe",
            "parakeetx-macos-portable-arm64-1.1.0.zip",
            "parakeetx-macos-installer-arm64-1.1.0.dmg",
        }
        actual = {
            release.artifact_name(platform, "1.1.0", extension)
            for platform, formats in release.FORMATS.items()
            for extension in formats
        }
        self.assertEqual(actual, expected)

    def test_debug_archive_is_distinct_from_release(self):
        for platform, extension in (("linux", ".tar.gz"), ("windows", ".zip"), ("macos", ".zip")):
            with self.subTest(platform=platform):
                release_name = release.artifact_name(platform, "1.1.0", extension)
                debug = release.artifact_name(platform, "1.1.0", extension, "Debug")
                self.assertEqual(debug, release_name.removesuffix(extension) + "-debug" + extension)

    def test_incomplete_release_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.populate(directory)
            next(directory.glob("*.dmg")).unlink()
            with self.assertRaisesRegex(RuntimeError, "Missing"):
                release.verify_release(directory, "0.1.0")

    def test_corrupt_package_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.populate(directory)
            next(directory.glob("*.deb")).write_bytes(b"modified package")
            with self.assertRaisesRegex(RuntimeError, "Checksum mismatch"):
                release.verify_release(directory, "0.1.0")

    def test_wrong_tag_or_unexpected_asset_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.populate(directory)
            with self.assertRaisesRegex(RuntimeError, "does not match"):
                release.verify_release(directory, "0.1.0", "v0.2.0")
            (directory / "old-release.deb").write_bytes(b"stale")
            with self.assertRaisesRegex(RuntimeError, "unexpected"):
                release.verify_release(directory, "0.1.0")

    def test_manifest_covers_every_package_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.populate(directory)
            checksums = release.verify_release(directory, "0.1.0", "v0.1.0")
            self.assertEqual(len(checksums.splitlines()), 8)
            (directory / "checksums.txt").write_text(checksums)
            self.assertEqual(release.verify_release(directory, "0.1.0", "0.1.0"), checksums)
            for line in checksums.splitlines():
                digest, name = line.split("  ")
                self.assertEqual(hashlib.sha256((directory / name).read_bytes()).hexdigest(), digest)

    def test_only_strong_glibc_imports_set_compatibility_floor(self):
        symbols = """
  1: 00000000 0 FUNC GLOBAL DEFAULT UND malloc@GLIBC_2.2.5
  2: 00000000 0 FUNC GLOBAL DEFAULT UND __libc_start_main@GLIBC_2.34
  3: 00000000 0 FUNC WEAK DEFAULT UND pidfd_spawnp@GLIBC_2.39
"""
        self.assertEqual(release.required_glibc(symbols), (2, 34))
        self.assertEqual(
            release.required_glibc(symbols + " 4: 00000000 0 FUNC GLOBAL DEFAULT UND acosf@GLIBC_2.43\n"), (2, 43)
        )


if __name__ == "__main__":
    unittest.main()
