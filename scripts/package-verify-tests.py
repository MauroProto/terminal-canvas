#!/usr/bin/env python3
"""Small archive fixtures exercise distribution gates without Cargo builds."""

import hashlib
import importlib.util
import io
import os
from pathlib import Path
import stat
import struct
import sys
import tarfile
import tempfile
import unittest
import warnings
import zipfile

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("package_verify", Path(__file__).with_name("package-verify.py"))
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


def pe(machine=0x8664):
    data = bytearray(90)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\0\0"
    struct.pack_into("<H", data, 68, machine)
    struct.pack_into("<H", data, 88, 0x20B)
    return bytes(data)


def elf(machine=62):
    data = bytearray(64)
    data[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", data, 18, machine)
    return bytes(data)


class PackageValidationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tc-package-fixture-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.version = "1.2.3"

    def checksum(self, archive, name=None):
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        Path(str(archive) + ".sha256").write_text(f"{digest}  {name or archive.name}\n", encoding="utf-8")

    def archive(self, platform="windows", mutate=None, mode=0o755):
        root = f"TerminalCanvas-{self.version}-{platform}-x86_64"
        binary = pe() if platform == "windows" else elf()
        files = [(root + "/" + name, binary) for name in verify.package_files(platform)[0]]
        files += [(root + "/LICENSE", (verify.REPO / "LICENSE").read_bytes()), (root + "/PORTABLE.md", (verify.REPO / "docs/PORTABLE.md").read_bytes())]
        if mutate:
            mutate(files, root)
        extension = ".zip" if platform == "windows" else ".tar.gz"
        archive = self.root / (root + extension)
        if platform == "windows":
            with zipfile.ZipFile(archive, "w") as target, warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                for name, content in files:
                    info = zipfile.ZipInfo(name)
                    info.external_attr = (stat.S_IFREG | mode) << 16
                    target.writestr(info, content)
        else:
            with tarfile.open(archive, "w:gz") as target:
                for name, content in files:
                    info = tarfile.TarInfo(name)
                    info.size, info.mode = len(content), mode
                    target.addfile(info, io.BytesIO(content))
        self.checksum(archive)
        return archive

    def validate(self, archive, platform="windows"):
        target = "x86_64-pc-windows-msvc" if platform == "windows" else "x86_64-unknown-linux-gnu"
        return verify.verify_portable(archive, target, self.version, self.root / "extracted")

    def test_complete_windows_zip(self):
        directory = self.validate(self.archive())
        self.assertTrue((directory / "tc-memory-mcp.exe").is_file())

    def test_complete_linux_tar_preserves_executable_mode(self):
        directory = self.validate(self.archive("linux"), "linux")
        self.assertEqual((directory / "mi-terminal-daemon").read_bytes(), elf())
        if os.name != "nt":
            self.assertTrue((directory / "mi-terminal-daemon").stat().st_mode & 0o111)

    def test_missing_helper_is_rejected_before_extracting(self):
        archive = self.archive(mutate=lambda files, root: files.pop(0))
        with self.assertRaisesRegex(ValueError, "Incomplete package"):
            self.validate(archive)
        self.assertFalse((self.root / "extracted").exists())

    def test_unexpected_file_is_rejected(self):
        archive = self.archive(mutate=lambda files, root: files.append((root + "/token.txt", b"unwanted")))
        with self.assertRaises(ValueError):
            self.validate(archive)

    def test_parent_traversal_is_rejected_before_extracting(self):
        archive = self.archive(mutate=lambda files, root: files.__setitem__(0, (root + "/../outside.exe", pe())))
        with self.assertRaisesRegex(ValueError, "Unsafe archive path"):
            self.validate(archive)
        self.assertFalse((self.root / "extracted").exists())

    def test_absolute_path_is_rejected(self):
        archive = self.archive(mutate=lambda files, root: files.__setitem__(0, ("/outside.exe", pe())))
        with self.assertRaisesRegex(ValueError, "Unsafe archive path"):
            self.validate(archive)

    def test_case_insensitive_duplicate_is_rejected(self):
        archive = self.archive(mutate=lambda files, root: files.__setitem__(1, (root + "/MI-TERMINAL.EXE", pe())))
        with self.assertRaisesRegex(ValueError, "Duplicate archive path"):
            self.validate(archive)

    def test_wrong_architecture_is_rejected(self):
        archive = self.archive(mutate=lambda files, root: files.__setitem__(0, (files[0][0], pe(0xAA64))))
        with self.assertRaisesRegex(ValueError, "Wrong architecture"):
            self.validate(archive)

    def test_missing_linux_executable_mode_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "Missing executable mode"):
            self.validate(self.archive("linux", mode=0o644), "linux")

    def test_changed_checksum_is_rejected(self):
        archive = self.archive()
        archive.write_bytes(archive.read_bytes() + b"changed")
        with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
            self.validate(archive)

    def test_checksum_for_another_filename_is_rejected(self):
        archive = self.archive()
        self.checksum(archive, "some-other-package.zip")
        with self.assertRaisesRegex(ValueError, "Checksum filename"):
            self.validate(archive)

    def test_tar_symlink_is_rejected(self):
        root = f"TerminalCanvas-{self.version}-linux-x86_64"
        archive = self.root / (root + ".tar.gz")
        with tarfile.open(archive, "w:gz") as target:
            link = tarfile.TarInfo(root + "/mi-terminal")
            link.type, link.linkname = tarfile.SYMTYPE, "/outside"
            target.addfile(link)
        self.checksum(archive)
        with self.assertRaisesRegex(ValueError, "Special archive entry"):
            self.validate(archive, "linux")

    def test_zip_symlink_is_rejected(self):
        archive = self.archive()
        with zipfile.ZipFile(archive, "r") as source:
            members = [(item, source.read(item)) for item in source.infolist()]
        with zipfile.ZipFile(archive, "w") as target:
            for index, (item, content) in enumerate(members):
                if index == 0:
                    item.external_attr = (stat.S_IFLNK | 0o777) << 16
                target.writestr(item, content)
        self.checksum(archive)
        with self.assertRaisesRegex(ValueError, "Archive symlink"):
            self.validate(archive)

    def test_release_requires_all_five_packages(self):
        with self.assertRaisesRegex(ValueError, "incomplete"):
            verify.verify_release_set(self.root, self.version)
        for name in ("macos-x86_64.dmg", "macos-aarch64.dmg", "windows-x86_64.zip", "windows-x86_64-setup.exe", "linux-x86_64.tar.gz"):
            archive = self.root / f"TerminalCanvas-{self.version}-{name}"
            archive.write_bytes(b"fixture artifact")
            self.checksum(archive)
        verify.verify_release_set(self.root, self.version)
        (self.root / "unexpected.sha256").write_text("unexpected", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "unexpected"):
            verify.verify_release_set(self.root, self.version)


if __name__ == "__main__":
    unittest.main()
