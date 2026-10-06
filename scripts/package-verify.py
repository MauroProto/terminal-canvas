#!/usr/bin/env python3
"""Validate the files users download, using only Python's standard library."""

import argparse
from contextlib import contextmanager
import hashlib
from itertools import islice
import json
import os
import platform as host_platform
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

REPO = Path(__file__).resolve().parent.parent
MAX_ZIP_METADATA = 2 * 1024 * 1024
TARGETS = {
    "x86_64-pc-windows-msvc": ("windows", "x86_64"),
    "aarch64-pc-windows-msvc": ("windows", "aarch64"),
    "x86_64-unknown-linux-gnu": ("linux", "x86_64"),
    "aarch64-unknown-linux-gnu": ("linux", "aarch64"),
    "x86_64-apple-darwin": ("macos", "x86_64"),
    "aarch64-apple-darwin": ("macos", "aarch64"),
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def cargo_version():
    with (REPO / "Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["package"]["version"]
    require(re.fullmatch(r"\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?", version), "Invalid package version")
    return version


def verify_checksum(archive):
    lines = Path(str(archive) + ".sha256").read_text(encoding="utf-8").splitlines()
    require(len(lines) == 1, "Checksum must contain exactly one entry")
    match = re.fullmatch(r"([0-9a-fA-F]{64})  (.+)", lines[0])
    require(match is not None and match[2] == archive.name, "Checksum filename does not match archive")
    with archive.open("rb") as source:
        actual = hashlib.file_digest(source, "sha256").hexdigest()
    require(actual == match[1].lower(), f"Checksum mismatch: {archive.name}")


def package_files(platform):
    suffix = ".exe" if platform == "windows" else ""
    binaries = [name + suffix for name in ("mi-terminal", "tc-memory", "tc-memory-mcp")]
    if platform == "linux":
        binaries.append("mi-terminal-daemon")
    return binaries, set(binaries + ["LICENSE", "PORTABLE.md"])


def validate_member(name, root, seen):
    path = PurePosixPath(name)
    require("\\" not in name and ":" not in name, f"Invalid archive path: {name}")
    require(not path.is_absolute() and ".." not in path.parts, f"Unsafe archive path: {name}")
    require(path.parts and path.parts[0] == root and len(path.parts) <= 2, f"Unexpected archive path: {name}")
    key = str(path).casefold()
    require(key not in seen, f"Duplicate archive path: {name}")
    seen.add(key)
    return path


def preflight_zip(source, maximum_members):
    """Bound ZIP metadata before ZipFile eagerly constructs its member list."""
    source.seek(0, os.SEEK_END)
    file_size = source.tell()

    def read_at(offset, size):
        require(0 <= offset <= file_size and 0 <= size <= file_size - offset,
                "Invalid ZIP metadata offset")
        source.seek(offset)
        data = source.read(size)
        require(len(data) == size, "Truncated ZIP metadata")
        return data

    require(file_size >= 22, "Invalid ZIP footer")
    tail_size = min(file_size, 65535 + 22)
    tail = read_at(file_size - tail_size, tail_size)
    # Match ZipFile's last-footer selection, including its no-comment shortcut.
    index = len(tail) - 22 if tail[-22:-18] == b"PK\x05\x06" and tail[-2:] == b"\0\0" else tail.rfind(b"PK\x05\x06")
    require(index >= 0 and index + 22 <= len(tail), "Invalid ZIP footer")
    footer_offset = file_size - tail_size + index
    _, disk, directory_disk, disk_members, members, directory_size, directory_offset, comment_size = struct.unpack(
        "<4s4H2IH", tail[index:index + 22])
    require(footer_offset + 22 + comment_size == file_size, "Invalid ZIP footer comment")
    require(disk == directory_disk == 0, "Multi-disk ZIP is unsupported")
    directory_end = footer_offset

    locator_offset = footer_offset - 20
    locator = read_at(locator_offset, 20) if locator_offset >= 0 else b""
    needs_zip64 = disk_members == 0xFFFF or members == 0xFFFF or directory_size == 0xFFFFFFFF or directory_offset == 0xFFFFFFFF
    require(not needs_zip64 or locator[:4] == b"PK\x06\x07", "Missing ZIP64 locator")
    if locator[:4] == b"PK\x06\x07":
        _, locator_disk, zip64_offset, disks = struct.unpack("<4sIQI", locator)
        require(locator_disk == 0 and disks == 1, "Multi-disk ZIP64 is unsupported")
        # Python 3.11/3.12 consume this fixed record immediately before the
        # locator. Reject layouts that another reader could interpret differently.
        record_offset = locator_offset - 56
        require(zip64_offset == record_offset, "Invalid ZIP64 metadata offset")
        record = read_at(record_offset, 56)
        require(record[:4] == b"PK\x06\x06", "Invalid ZIP64 footer")
        _, record_size, _, _, disk64, directory_disk64, disk_members64, members64, directory_size64, directory_offset64 = struct.unpack(
            "<4sQ2H2I4Q", record)
        require(record_size == 44, "Unsupported ZIP64 extensible metadata")
        require(record_offset + 12 + record_size == locator_offset, "Invalid ZIP64 metadata offset")
        require(disk64 == directory_disk64 == 0, "Multi-disk ZIP64 is unsupported")
        require(members64 <= maximum_members, "Unexpected archive entries")
        require(directory_size64 <= MAX_ZIP_METADATA, "Oversized ZIP central directory")
        for original, expanded, sentinel in (
                (disk_members, disk_members64, 0xFFFF), (members, members64, 0xFFFF),
                (directory_size, directory_size64, 0xFFFFFFFF), (directory_offset, directory_offset64, 0xFFFFFFFF)):
            require(original == sentinel or original == expanded, "Inconsistent ZIP64 footer")
        disk_members, members = disk_members64, members64
        directory_size, directory_offset = directory_size64, directory_offset64
        directory_end = record_offset
        require(directory_offset + directory_size == zip64_offset, "Invalid ZIP64 directory offset")

    require(disk_members == members, "Inconsistent ZIP entry count")
    require(members <= maximum_members, "Unexpected archive entries")
    require(directory_size <= MAX_ZIP_METADATA, "Oversized ZIP central directory")
    directory_start = directory_end - directory_size
    require(0 <= directory_offset == directory_start, "Invalid ZIP directory offset")
    position, inspected = directory_start, 0
    while position < directory_end:
        require(inspected < maximum_members, "Unexpected archive entries")
        require(position + 46 <= directory_end, "Truncated ZIP central directory")
        header = read_at(position, 46)
        require(header[:4] == b"PK\x01\x02", "Invalid ZIP central directory")
        name_size, extra_size, member_comment_size, member_disk = struct.unpack_from("<4H", header, 28)
        require(member_disk == 0, "Multi-disk ZIP member is unsupported")
        position += 46 + name_size + extra_size + member_comment_size
        require(position <= directory_end, "Invalid ZIP central directory lengths")
        inspected += 1
    require(inspected == members, "Inconsistent ZIP entry count")


@contextmanager
def open_bounded_zip(archive, maximum_members):
    # Use the same descriptor for the preflight and parser, avoiding a reopen.
    with archive.open("rb") as source:
        preflight_zip(source, maximum_members)
        with zipfile.ZipFile(source) as handle:
            yield handle


def extract_portable(archive, destination, root, platform):
    """Validate the complete manifest before writing any archive entries."""
    _, expected = package_files(platform)
    seen, actual, entries = set(), set(), []
    if platform == "windows":
        handle = open_bounded_zip(archive, len(expected) + 1)
    else:
        handle = tarfile.open(archive, "r:gz")
    with handle as handle:
        # Inspect only enough TAR headers to detect an oversized manifest.
        # getmembers() would parse and retain the entire archive before the gate.
        members = handle.infolist() if platform == "windows" else list(islice(handle, len(expected) + 2))
        require(len(members) <= len(expected) + 1, "Unexpected archive entries")
        for member in members:
            name = member.filename if platform == "windows" else member.name
            path = validate_member(name, root, seen)
            if platform == "windows":
                mode = member.external_attr >> 16
                directory = member.is_dir()
                require(not stat.S_ISLNK(mode), f"Archive symlink: {name}")
                require(stat.S_IFMT(mode) in (0, stat.S_IFREG, stat.S_IFDIR), f"Special archive entry: {name}")
                require(not member.flag_bits & 1, "Encrypted archive is unsupported")
                size = member.file_size
            else:
                mode, directory, size = member.mode, member.isdir(), member.size
                require(member.isfile() or directory, f"Special archive entry: {name}")
            if directory:
                require(len(path.parts) == 1, f"Unexpected directory: {name}")
                continue
            require(len(path.parts) == 2 and path.name in expected, f"Unexpected file: {name}")
            require(0 < size <= 512 * 1024 * 1024, f"Empty or oversized file: {name}")
            if platform == "linux" and path.name not in ("LICENSE", "PORTABLE.md"):
                require(mode & 0o111, f"Missing executable mode: {name}")
            actual.add(path.name)
            entries.append((member, path, mode))
        require(actual == expected, f"Incomplete package: missing {sorted(expected - actual)}")
        require(not (destination / root).exists(), "Extraction destination already contains this package")
        for member, path, mode in entries:
            output = destination.joinpath(*path.parts)
            output.parent.mkdir(parents=True, exist_ok=True)
            source = handle.open(member) if platform == "windows" else handle.extractfile(member)
            with source, output.open("xb") as target:
                shutil.copyfileobj(source, target)
            if platform == "linux":
                output.chmod(mode & 0o755)
    return destination / root


def verify_binary(path, platform, arch):
    with path.open("rb") as source:
        header = source.read(64)
        if platform == "windows":
            require(header[:2] == b"MZ" and len(header) == 64, f"Invalid PE binary: {path.name}")
            source.seek(struct.unpack_from("<I", header, 60)[0])
            pe = source.read(26)
            require(len(pe) == 26 and pe[:4] == b"PE\0\0", f"Invalid PE header: {path.name}")
            machine = 0x8664 if arch == "x86_64" else 0xAA64
            require(struct.unpack_from("<H", pe, 4)[0] == machine, f"Wrong architecture: {path.name}")
            require(struct.unpack_from("<H", pe, 24)[0] == 0x20B, f"Expected PE32+ binary: {path.name}")
        elif platform == "linux":
            require(header[:6] == b"\x7fELF\x02\x01" and len(header) == 64, f"Expected ELF64 binary: {path.name}")
            machine = 62 if arch == "x86_64" else 183
            require(struct.unpack_from("<H", header, 18)[0] == machine, f"Wrong architecture: {path.name}")
        else:
            subprocess.run(["lipo", str(path), "-verify_arch", "x86_64" if arch == "x86_64" else "arm64"], check=True, timeout=30)


def smoke_helpers(directory, version, platform=None, arch=None):
    suffix = ".exe" if os.name == "nt" else ""
    with tempfile.TemporaryDirectory(prefix="tc-package-memory-") as isolated:
        env = os.environ.copy()
        env.update(TERMINAL_CANVAS_HOME=isolated, TC_MEMORY_DB=str(Path(isolated) / "memory.db"), TC_MEMORY_ROOT=isolated)
        # A user's task identity must not leak into a package test.
        env.pop("TC_MEMORY_TASK_ID", None)
        if platform is None:
            platform = {"Windows": "windows", "Linux": "linux", "Darwin": "macos"}[host_platform.system()]
        if arch is None:
            arch = {"amd64": "x86_64", "arm64": "aarch64"}.get(host_platform.machine().lower(), host_platform.machine().lower())
        app = directory / ("TerminalCanvas" if platform == "macos" else "mi-terminal" + suffix)
        result = subprocess.run([str(app), "--version"], capture_output=True, text=True, env=env, cwd=isolated, timeout=30, check=True)
        require(result.stdout.strip() == f"TerminalCanvas {version}", "Packaged application version differs from Cargo")
        result = subprocess.run([str(app), "--health-check"], capture_output=True, text=True, env=env, cwd=isolated, timeout=30, check=True)
        report = json.loads(result.stdout)
        require(report.get("ok") is True and report.get("version") == version, "Packaged application health failed")
        require(report.get("os") == platform and report.get("arch") == arch and report.get("daemon") == (platform != "windows"), "Packaged application target/daemon differs from expected build")
        require(report.get("profile", {}).get("isolated") is True and report["profile"].get("global_agent_configuration") is False, "Package smoke test profile was not isolated")
        for name in ("config", "data", "cache", "panic_log"):
            require(Path(report["profile"][name]).resolve().is_relative_to(Path(isolated).resolve()), f"Profile path escaped isolation: {name}")
        helper = directory / ("tc-memory" + suffix)
        result = subprocess.run([str(helper), "--help"], capture_output=True, text=True, env=env, cwd=isolated, timeout=30, check=True)
        require("tc-memory" in result.stdout, "Memory helper returned unexpected help")
        result = subprocess.run([str(helper), "health"], capture_output=True, text=True, env=env, cwd=isolated, timeout=30, check=True)
        require(json.loads(result.stdout).get("ok") is True, "Packaged memory helper health failed")
        messages = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "package-verify", "version": "1"}}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        ]
        result = subprocess.run([str(directory / ("tc-memory-mcp" + suffix))], input="".join(json.dumps(message) + "\n" for message in messages), capture_output=True, text=True, env=env, cwd=isolated, timeout=30, check=True)
        responses = [json.loads(line) for line in result.stdout.splitlines()]
        require(len(responses) == 2 and all(response.get("jsonrpc") == "2.0" for response in responses), "Invalid packaged MCP responses")
        initialized, tools = responses
        require(initialized.get("id") == 1 and initialized.get("result", {}).get("serverInfo", {}).get("version") == version, "Packaged MCP version differs from Cargo version")
        require(tools.get("id") == 2 and tools.get("result", {}).get("tools"), "Packaged MCP did not expose tools")


def verify_portable(archive, target, version, destination, smoke=False):
    platform, arch = TARGETS[target]
    require(platform in ("windows", "linux"), "Expected a portable target")
    root = f"TerminalCanvas-{version}-{platform}-{arch}"
    extension = ".zip" if platform == "windows" else ".tar.gz"
    require(archive.name == root + extension, "Archive name differs from expected version/target")
    verify_checksum(archive)
    directory = extract_portable(archive, destination, root, platform)
    for binary in package_files(platform)[0]:
        verify_binary(directory / binary, platform, arch)
    for file, original in (("LICENSE", REPO / "LICENSE"), ("PORTABLE.md", REPO / "docs/PORTABLE.md")):
        require((directory / file).read_bytes() == original.read_bytes(), f"Packaged {file} differs from source")
    if smoke:
        smoke_helpers(directory, version, platform, arch)
    return directory


def verify_signature(path, team_id):
    subprocess.run(["codesign", "--verify", "--strict", "--verbose=2", str(path)], check=True, timeout=30)
    signature = subprocess.run(["codesign", "--display", "--verbose=4", str(path)], check=True, capture_output=True, text=True, timeout=30).stderr
    require(f"TeamIdentifier={team_id}" in signature.splitlines(), f"Unexpected signing team: {path.name}")
    require("Authority=Developer ID Application:" in signature and "runtime" in signature, f"Missing Developer ID/hardened runtime: {path.name}")


def verify_dmg(archive, target, version, signed, team_id):
    platform, arch = TARGETS[target]
    require(platform == "macos" and archive.name == f"TerminalCanvas-{version}-macos-{arch}.dmg", "Unexpected DMG target/name")
    require(not signed or re.fullmatch(r"[A-Z0-9]{10}", team_id or ""), "A real Apple team ID is required")
    verify_checksum(archive)
    if signed:
        subprocess.run(["xcrun", "stapler", "validate", str(archive)], check=True, timeout=60)
        subprocess.run(["spctl", "--assess", "--type", "open", "--context", "context:primary-signature", "--verbose=2", str(archive)], check=True, timeout=60)
    with tempfile.TemporaryDirectory(prefix="tc-package-dmg-") as mount:
        subprocess.run(["hdiutil", "attach", str(archive), "-nobrowse", "-readonly", "-mountpoint", mount], check=True, timeout=60)
        try:
            app = Path(mount) / "TerminalCanvas.app"
            with (app / "Contents/Info.plist").open("rb") as source:
                info = plistlib.load(source)
            for key, expected in {"CFBundleExecutable": "TerminalCanvas", "CFBundleIdentifier": "com.terminalcanvas.app", "CFBundleShortVersionString": version, "CFBundleVersion": version, "CFBundlePackageType": "APPL"}.items():
                require(info.get(key) == expected, f"Incorrect bundle metadata: {key}")
            require((app / "Contents/Resources/TerminalCanvas.icns").stat().st_size > 0, "Missing bundle icon")
            binaries = app / "Contents/MacOS"
            expected = {"TerminalCanvas", "mi-terminal-daemon", "tc-memory", "tc-memory-mcp"}
            require({path.name for path in binaries.iterdir()} == expected, "Incomplete or unexpected bundle binaries")
            for binary in expected:
                path = binaries / binary
                require(path.is_file() and not path.is_symlink() and os.access(path, os.X_OK), f"Missing executable: {binary}")
                verify_binary(path, "macos", arch)
                if signed:
                    verify_signature(path, team_id)
            if signed:
                verify_signature(app, team_id)
                subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True, timeout=30)
                subprocess.run(["spctl", "--assess", "--type", "execute", "--verbose=2", str(app)], check=True, timeout=60)
            smoke_helpers(binaries, version, platform, arch)
        finally:
            subprocess.run(["hdiutil", "detach", mount], check=True, timeout=60)


def verify_release_set(directory, version):
    names = {
        f"TerminalCanvas-{version}-macos-x86_64.dmg",
        f"TerminalCanvas-{version}-macos-aarch64.dmg",
        f"TerminalCanvas-{version}-windows-x86_64.zip",
        f"TerminalCanvas-{version}-windows-x86_64-setup.exe",
        f"TerminalCanvas-{version}-linux-x86_64.tar.gz",
    }
    expected = names | {name + ".sha256" for name in names}
    require({path.name for path in directory.iterdir()} == expected, "Release assets are incomplete or contain unexpected files")
    for name in sorted(names):
        archive = directory / name
        require(archive.is_file() and not archive.is_symlink() and archive.stat().st_size > 0, f"Invalid release asset: {name}")
        verify_checksum(archive)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--archive", type=Path)
    mode.add_argument("--dmg", type=Path)
    mode.add_argument("--release-dir", type=Path)
    mode.add_argument("--smoke-directory", type=Path)
    mode.add_argument("--version-only", action="store_true")
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--version")
    parser.add_argument("--extract-to", type=Path)
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--require-signature", action="store_true")
    parser.add_argument("--team-id")
    args = parser.parse_args()
    version = args.version or cargo_version()
    require(re.fullmatch(r"\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?", version), "Invalid version")
    if args.version_only:
        print(version)
        return
    if args.release_dir:
        verify_release_set(args.release_dir.resolve(), version)
    elif args.smoke_directory:
        smoke_helpers(args.smoke_directory.resolve(), version)
    elif args.dmg:
        require(args.target is not None, "--target is required")
        verify_dmg(args.dmg.resolve(), args.target, version, args.require_signature, args.team_id)
    else:
        require(args.target is not None, "--target is required")
        if args.extract_to:
            require(not args.extract_to.exists(), "--extract-to must be a new directory")
            args.extract_to.mkdir(parents=True)
            verify_portable(args.archive.resolve(), args.target, version, args.extract_to.resolve(), args.smoke)
        else:
            with tempfile.TemporaryDirectory(prefix="tc-package-verify-") as isolated:
                verify_portable(args.archive.resolve(), args.target, version, Path(isolated), args.smoke)
    print("Package verification passed")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError, zipfile.BadZipFile, tarfile.TarError) as error:
        raise SystemExit(f"Package verification failed: {error}") from error
