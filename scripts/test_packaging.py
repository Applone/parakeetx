import hashlib
import importlib.util
import os
import pathlib
import tempfile
import unittest

import installers
import package


SPEC = importlib.util.spec_from_file_location("verify_release", pathlib.Path(__file__).with_name("verify-release.py"))
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


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
            package.zip_directory(stage, archive)
            package.verify_archive(archive, stage.name, "linux", "0.1.0")

    def populate(self, directory):
        for target, formats in VERIFY.FORMATS.items():
            for extension in formats:
                path = directory / f"parakeetx-0.1.0-Release-{target}{extension}"
                path.write_bytes(b"a nonempty package fixture")
                package.write_checksum(path)

    def test_incomplete_release_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.populate(directory)
            next(directory.glob("*.dmg")).unlink()
            with self.assertRaisesRegex(RuntimeError, "Missing"):
                VERIFY.verify(directory, "0.1.0")

    def test_corrupt_package_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.populate(directory)
            next(directory.glob("*.deb")).write_bytes(b"modified package")
            with self.assertRaisesRegex(RuntimeError, "Checksum mismatch"):
                VERIFY.verify(directory, "0.1.0")

    def test_wrong_tag_or_unexpected_asset_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.populate(directory)
            with self.assertRaisesRegex(RuntimeError, "does not match"):
                VERIFY.verify(directory, "0.1.0", "v0.2.0")
            (directory / "old-release.deb").write_bytes(b"stale")
            with self.assertRaisesRegex(RuntimeError, "unexpected"):
                VERIFY.verify(directory, "0.1.0")

    def test_manifest_covers_every_package_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.populate(directory)
            checksums = VERIFY.verify(directory, "0.1.0", "v0.1.0")
            self.assertEqual(len(checksums.splitlines()), 8)
            (directory / "checksums.txt").write_text(checksums)
            self.assertEqual(VERIFY.verify(directory, "0.1.0", "0.1.0"), checksums)
            for line in checksums.splitlines():
                digest, name = line.split("  ")
                self.assertEqual(hashlib.sha256((directory / name).read_bytes()).hexdigest(), digest)

    def test_only_strong_glibc_imports_set_compatibility_floor(self):
        symbols = """
  1: 00000000 0 FUNC GLOBAL DEFAULT UND malloc@GLIBC_2.2.5
  2: 00000000 0 FUNC GLOBAL DEFAULT UND __libc_start_main@GLIBC_2.34
  3: 00000000 0 FUNC WEAK DEFAULT UND pidfd_spawnp@GLIBC_2.39
"""
        self.assertEqual(installers.required_glibc(symbols), (2, 34))
        self.assertEqual(installers.required_glibc(symbols +
            " 4: 00000000 0 FUNC GLOBAL DEFAULT UND acosf@GLIBC_2.43\n"), (2, 43))


if __name__ == "__main__":
    unittest.main()
