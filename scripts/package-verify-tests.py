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
from unittest import mock
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

    def archive(self, platform="windows", mutate=None, mode=0o755, force_zip64=False):
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
                    if force_zip64:
                        with target.open(info, "w", force_zip64=True) as member:
                            member.write(content)
                    else:
                        target.writestr(info, content)
        else:
            with tarfile.open(archive, "w:gz") as target:
                for name, content in files:
                    info = tarfile.TarInfo(name)
                    info.size, info.mode = len(content), mode
                    target.addfile(info, io.BytesIO(content))
        self.checksum(archive)
        return archive

    def zip64_footer(self, archive, sentinels=True):
        raw = archive.read_bytes()
        footer = struct.unpack("<4s4H2IH", raw[-22:])
        record_offset = len(raw) - 22
        record = struct.pack("<4sQ2H2I4Q", b"PK\x06\x06", 44, 45, 45, 0, 0,
                             footer[3], footer[4], footer[5], footer[6])
        locator = struct.pack("<4sIQI", b"PK\x06\x07", 0, record_offset, 1)
        if sentinels:
            end = struct.pack("<4s4H2IH", b"PK\x05\x06", 0, 0, 0xFFFF, 0xFFFF,
                              0xFFFFFFFF, 0xFFFFFFFF, 0)
        else:
            end = raw[-22:]
        archive.write_bytes(raw[:-22] + record + locator + end)
        self.checksum(archive)
        return record_offset

    def reject_before_zipfile(self, archive, message):
        with mock.patch.object(verify.zipfile, "ZipFile", side_effect=AssertionError("ZipFile parsed before the gate")):
            with self.assertRaisesRegex(ValueError, message):
                self.validate(archive)
        self.assertFalse((self.root / "extracted").exists())

    def validate(self, archive, platform="windows"):
        target = "x86_64-pc-windows-msvc" if platform == "windows" else "x86_64-unknown-linux-gnu"
        return verify.verify_portable(archive, target, self.version, self.root / "extracted")

    def test_complete_windows_zip(self):
        directory = self.validate(self.archive())
        self.assertTrue((directory / "tc-memory-mcp.exe").is_file())

    def test_complete_zip_with_comment(self):
        archive = self.archive()
        with zipfile.ZipFile(archive, "a") as target:
            target.comment = b"portable package comment " + b"x" * 4096
        self.checksum(archive)
        self.assertEqual((self.validate(archive) / "mi-terminal.exe").read_bytes(), pe())

    def test_complete_zip64_local_headers_and_footer(self):
        archive = self.archive(force_zip64=True)
        self.zip64_footer(archive)
        with zipfile.ZipFile(archive) as target:
            self.assertIsNone(target.testzip())
            self.assertEqual(len(target.infolist()), 5)
        self.assertEqual((self.validate(archive) / "mi-terminal.exe").read_bytes(), pe())

    def test_complete_zip64_footer_without_normal_sentinels(self):
        archive = self.archive()
        self.zip64_footer(archive, sentinels=False)
        self.assertEqual((self.validate(archive) / "tc-memory.exe").read_bytes(), pe())

    def test_complete_zip_with_optional_package_directory(self):
        archive = self.archive()
        with zipfile.ZipFile(archive, "a") as target:
            target.writestr(f"TerminalCanvas-{self.version}-windows-x86_64/", b"")
        self.checksum(archive)
        self.assertEqual((self.validate(archive) / "mi-terminal.exe").read_bytes(), pe())

    def test_large_zip_manifest_rejected_before_constructing_zipfile(self):
        archive = self.archive(mutate=lambda files, root: files.extend(
            (root + f"/unexpected-{index}", b"x") for index in range(3000)
        ))
        self.reject_before_zipfile(archive, "Unexpected archive entries")

    def test_forged_small_zip_entry_count_does_not_bypass_scan(self):
        archive = self.archive(mutate=lambda files, root: files.extend(
            (root + f"/unexpected-{index}", b"x") for index in range(3000)
        ))
        raw = bytearray(archive.read_bytes())
        struct.pack_into("<2H", raw, len(raw) - 22 + 8, 5, 5)
        archive.write_bytes(raw)
        self.checksum(archive)
        self.reject_before_zipfile(archive, "Unexpected archive entries")

    def test_zip64_override_is_checked_without_normal_sentinels(self):
        for field, offset, value, message in (
                ("entries", 32, 3005, "Unexpected archive entries"),
                ("metadata", 40, verify.MAX_ZIP_METADATA + 1, "Oversized ZIP central directory")):
            with self.subTest(field=field):
                archive = self.archive()
                record_offset = self.zip64_footer(archive, sentinels=False)
                raw = bytearray(archive.read_bytes())
                struct.pack_into("<Q", raw, record_offset + offset, value)
                archive.write_bytes(raw)
                self.checksum(archive)
                self.reject_before_zipfile(archive, message)

    def test_invalid_zip_offsets_and_disks_rejected_before_parser(self):
        for field, offset, fmt, value, message in (
                ("offset", 16, "<I", 0xFFFFFFFE, "Invalid ZIP directory offset"),
                ("disk", 4, "<H", 1, "Multi-disk ZIP"),
                ("directory_disk", 6, "<H", 1, "Multi-disk ZIP"),
                ("metadata", 12, "<I", verify.MAX_ZIP_METADATA + 1, "Oversized ZIP central directory"),
                ("missing_zip64", 8, "<2H", (0xFFFF, 0xFFFF), "Missing ZIP64 locator")):
            with self.subTest(field=field):
                archive = self.archive()
                raw = bytearray(archive.read_bytes())
                values = value if isinstance(value, tuple) else (value,)
                struct.pack_into(fmt, raw, len(raw) - 22 + offset, *values)
                archive.write_bytes(raw)
                self.checksum(archive)
                self.reject_before_zipfile(archive, message)

    def test_invalid_zip64_offsets_and_disks_rejected_before_parser(self):
        for field, relative_offset, fmt, value, message in (
                ("locator_offset", 56 + 8, "<Q", 0xFFFFFFFFFFFFFFFF, "Invalid ZIP64 metadata offset"),
                ("locator_disks", 56 + 16, "<I", 2, "Multi-disk ZIP64"),
                ("disk", 16, "<I", 1, "Multi-disk ZIP64"),
                ("directory_offset", 48, "<Q", 0xFFFFFFFFFFFFFFFF, "Invalid ZIP64 directory offset"),
                ("extensible_data", 4, "<Q", 45, "Unsupported ZIP64 extensible metadata")):
            with self.subTest(field=field):
                archive = self.archive()
                record_offset = self.zip64_footer(archive)
                raw = bytearray(archive.read_bytes())
                struct.pack_into(fmt, raw, record_offset + relative_offset, value)
                archive.write_bytes(raw)
                self.checksum(archive)
                self.reject_before_zipfile(archive, message)

    def test_zip64_gap_cannot_change_which_record_the_parser_reads(self):
        archive = self.archive()
        record_offset = self.zip64_footer(archive)
        raw = archive.read_bytes()
        locator_offset = record_offset + 56
        archive.write_bytes(raw[:locator_offset] + raw[record_offset:locator_offset] + raw[locator_offset:])
        self.checksum(archive)
        self.reject_before_zipfile(archive, "Invalid ZIP64 metadata offset")

    def test_zip_footer_comment_and_trailing_data_must_match(self):
        for field in ("comment", "trailing_data"):
            with self.subTest(field=field):
                archive = self.archive()
                raw = bytearray(archive.read_bytes())
                if field == "comment":
                    struct.pack_into("<H", raw, len(raw) - 2, 10)
                else:
                    raw.extend(b"unexpected trailing bytes")
                archive.write_bytes(raw)
                self.checksum(archive)
                self.reject_before_zipfile(archive, "Invalid ZIP footer comment")

    def test_central_directory_lengths_cannot_escape_metadata_bounds(self):
        archive = self.archive()
        raw = bytearray(archive.read_bytes())
        directory_offset = struct.unpack_from("<I", raw, len(raw) - 22 + 16)[0]
        struct.pack_into("<H", raw, directory_offset + 28, 0xFFFF)
        archive.write_bytes(raw)
        self.checksum(archive)
        self.reject_before_zipfile(archive, "Invalid ZIP central directory lengths")

    def test_complete_linux_tar_preserves_executable_mode(self):
        directory = self.validate(self.archive("linux"), "linux")
        self.assertEqual((directory / "mi-terminal-daemon").read_bytes(), elf())
        if os.name != "nt":
            self.assertTrue((directory / "mi-terminal-daemon").stat().st_mode & 0o111)

    def test_large_tar_manifest_is_rejected_without_parsing_all_members(self):
        archive = self.archive("linux", mutate=lambda files, root: files.extend(
            (root + f"/unexpected-{index}", b"x") for index in range(5000)
        ))
        read_members = set()
        original_next = tarfile.TarFile.next

        def counted_next(handle):
            member = original_next(handle)
            if member is not None:
                # TarFile.next() can return the already cached first member.
                read_members.add((member.offset, member.name))
            return member

        # Exercise the real compressed archive reader, counting parsed headers.
        with mock.patch.object(tarfile.TarFile, "next", counted_next):
            with self.assertRaisesRegex(ValueError, "Unexpected archive entries"):
                self.validate(archive, "linux")
        self.assertEqual(len(read_members), len(verify.package_files("linux")[1]) + 2)
        self.assertFalse((self.root / "extracted").exists())

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
