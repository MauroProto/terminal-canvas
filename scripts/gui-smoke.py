#!/usr/bin/env python3
"""Exercise the shipped native GUI on a disposable Linux Actions desktop.

This uses real X11 input, PTYs, disk writes, close callbacks and a second app
process. It never runs on a developer's desktop, uses accounts or starts an LLM.
The folder is seeded; native file-picker and non-Linux GUI QA remain separate.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import sys
import time
import unicodedata
import uuid


DEADLINE = 30
ROWS = 7000


def write_new(path, data):
    with path.open("x", encoding="utf-8") as file:
        file.write(data)


def wait_for(label, observe, timeout=DEADLINE):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = observe()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(f"Timed out waiting for {label}")


def command(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True,
                          timeout=15).stdout.strip()


def leaf_ids(node):
    if not node:
        return []
    if "Leaf" in node:
        return [node["Leaf"]["id"]]
    split = node["Split"]
    return leaf_ids(split["first"]) + leaf_ids(split["second"])


class Desktop:
    def __init__(self, binary, output):
        self.binary = binary
        self.output = output
        self.profile = output / "profile"
        self.project = output / "fixture project"
        self.layout_path = self.profile / "data" / "layout.json"
        self.profile.mkdir()
        (self.profile / "config").mkdir()
        (self.profile / "data").mkdir()
        self.project.mkdir()
        self.env = os.environ.copy()
        self.env.update(TERMINAL_CANVAS_HOME=str(self.profile),
                        SHELL="/bin/bash", RUST_LOG="info")
        self.process = None
        self.window = None
        self.starts = 0
        self.screenshots = 0
        self.data_mode = stat.S_IMODE((self.profile / "data").stat().st_mode)
        write_new(self.profile / "config" / "config.toml",
                  '[terminal]\nshell = "/bin/bash"\nscrollback_lines = 50000\n'
                  '[onboarding]\ndismissed = true\n')
        self.workspace_id = str(uuid.uuid4())
        write_new(self.layout_path, json.dumps({
            "schema_version": 3, "active_ws": 0, "sidebar_visible": True,
            "workspaces": [{"id": self.workspace_id, "name": "GUI fixture",
                            "cwd": str(self.project), "panels": []}],
        }))
        self.fixture = self.project / "fixture-output.py"
        write_new(self.fixture, """import os, pathlib, sys, time
if len(sys.argv) > 1:
    pathlib.Path(sys.argv[1]).write_text(str(os.getpid()))
    print('TC_GUI_STREAM_START', flush=True)
    for i in range(100000):
        print('TC_GUI_STREAM_%06d' % i, flush=True)
        time.sleep(0.01)
else:
    print('TC_GUI_LARGE_START', flush=True)
    for i in range(7000):
        print('TC_GUI_ROW_%05d_' % i + 'x' * 70)
    print('TC_GUI_LARGE_END', flush=True)
""")

    def layout(self):
        try:
            return json.loads(self.layout_path.read_text())
        except (OSError, json.JSONDecodeError):
            return None

    def panels(self):
        layout = self.layout()
        return layout["workspaces"][0]["panels"] if layout else []

    def root(self, root_id):
        return next((p for p in self.panels() if p["id"] == root_id), None)

    def start(self):
        self.starts += 1
        self.stdout = (self.output / f"app-{self.starts}.stdout").open("xb")
        self.stderr = (self.output / f"app-{self.starts}.stderr").open("xb")
        self.process = subprocess.Popen([str(self.binary)], cwd=self.project,
                                        env=self.env, stdout=self.stdout,
                                        stderr=self.stderr)

        def find_window():
            if self.process.poll() is not None:
                raise AssertionError(f"Native app exited early: {self.process.returncode}")
            found = subprocess.run(
                ["xdotool", "search", "--onlyvisible", "--pid", str(self.process.pid)],
                capture_output=True, text=True, timeout=5)
            windows = [w for w in found.stdout.splitlines()
                       if command("xdotool", "getwindowname", w).startswith("My Terminal")]
            assert len(windows) <= 1, f"Ambiguous app windows: {windows}"
            return windows[0] if windows else None

        self.window = wait_for("native WGPU window", find_window)
        command("xdotool", "windowactivate", "--sync", self.window)
        # Give the first renderer/logic passes a bounded warm-up, not a fixed
        # delay for any persistence assertion below.
        time.sleep(0.5)

    def key(self, key):
        assert self.process.poll() is None, "Native app is not alive"
        command("xdotool", "windowactivate", "--sync", self.window)
        command("xdotool", "key", "--clearmodifiers", key)

    def type(self, text):
        assert "\n" not in text and "\r" not in text
        command("xdotool", "type", "--clearmodifiers", "--delay", "2", "--", text)

    def shell_command(self, text):
        self.type(text)
        self.key("Return")

    def rename(self, title):
        self.key("F2")
        time.sleep(0.15)
        self.key("ctrl+a")
        self.type(title)
        self.key("Return")

    def screenshot(self, label):
        self.screenshots += 1
        path = self.output / f"screen-{self.screenshots:02d}-{label}.png"
        command("import", "-window", self.window, str(path))
        assert path.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
        return path

    def ocr(self, label):
        path = self.screenshot(label)
        text = command("tesseract", str(path), "stdout", "-l", "eng")
        write_new(path.with_suffix(".txt"), text)
        return " ".join(unicodedata.normalize("NFKD", text).encode(
            "ascii", "ignore").decode().lower().split())

    def export(self, label):
        directory = self.profile / "exports"
        existing = set(directory.glob("*.txt")) if directory.exists() else set()
        self.key("ctrl+shift+e")
        exported = wait_for("completed native export", lambda:
                            next(iter(set(directory.glob("*.txt")) - existing), None))
        def completed_export():
            first_size = exported.stat().st_size
            time.sleep(0.1)
            return exported.read_text() if first_size > 0 and first_size == exported.stat().st_size else None

        text = wait_for("stable nonempty export", completed_export)
        write_new(self.output / f"export-{label}.txt", text)
        return text

    def close(self):
        started = time.monotonic()
        self.key("alt+F4")
        code = self.process.wait(timeout=20)
        elapsed = time.monotonic() - started
        self.stdout.close()
        self.stderr.close()
        self.process = None
        assert code == 0, f"Native app close returned {code}"
        assert not (self.profile / "data" / "run.marker").exists(), "Clean close left recovery marker"
        return elapsed

    def cleanup(self):
        (self.profile / "data").chmod(self.data_mode)
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        for file in (getattr(self, "stdout", None), getattr(self, "stderr", None)):
            if file is not None:
                file.close()


def assert_large_history(text):
    assert "TC_GUI_LARGE_START" in text and "TC_GUI_LARGE_END" in text
    found = re.findall(r"TC_GUI_ROW_(\d{5})_", text)
    assert found == [f"{i:05d}" for i in range(ROWS)], "History lost or duplicated rows"
    assert len(text.encode()) > 512 * 1024, "Fixture must exceed the former byte budget"


def run(desktop, result):
    desktop.start()
    desktop.screenshot("empty-folder-workspace")
    desktop.key("ctrl+shift+t")
    root = wait_for("first persisted terminal", lambda:
                    desktop.panels()[0] if len(desktop.panels()) == 1
                    and desktop.panels()[0].get("runtime_session_id") else None)
    root_id = root["id"]
    desktop.shell_command(f"python3 {shlex.quote(str(desktop.fixture))}")
    wait_for("large PTY output retained", lambda: any(
        b"TC_GUI_LARGE_END" in p.read_bytes()
        for p in (desktop.profile / "data" / "scrollback").glob("*.mtlg")))
    assert_large_history(desktop.export("large-before-close"))
    desktop.rename("GUI Root")
    wait_for("rename saved", lambda: desktop.root(root_id).get("custom_title") == "GUI Root")
    desktop.key("ctrl+alt+d")
    wait_for("independent split leaf", lambda: len(leaf_ids(desktop.root(root_id).get("split_tree"))) == 2)
    desktop.shell_command("printf '\\nTC_GUI_RIGHT_ONLY\\n'")
    assert "TC_GUI_RIGHT_ONLY" in desktop.export("right-before-close")
    desktop.key("ctrl+shift+t")
    wait_for("second terminal", lambda: len(desktop.panels()) == 2)
    desktop.rename("GUI Other")
    wait_for("second rename saved", lambda: any(p.get("custom_title") == "GUI Other" for p in desktop.panels()))
    desktop.shell_command("printf '\\nTC_GUI_OTHER_ONLY\\n'")
    other = desktop.export("other-before-close")
    assert "TC_GUI_OTHER_ONLY" in other and "TC_GUI_RIGHT_ONLY" not in other
    command("xdotool", "windowminimize", desktop.window)
    command("xdotool", "windowactivate", "--sync", desktop.window)
    desktop.screenshot("two-panels-and-split")
    before = desktop.layout()
    write_new(desktop.output / "layout-before-close.json", json.dumps(before, indent=2))
    result["first_close_seconds"] = desktop.close()
    closed = desktop.layout()
    write_new(desktop.output / "layout-after-close.json", json.dumps(closed, indent=2))
    saved_root = desktop.root(root_id)
    saved_leaves = leaf_ids(saved_root["split_tree"])
    assert len(set(saved_leaves)) == 2
    assert set(saved_root["leaf_runtime_session_ids"]) == set(saved_leaves)
    assert closed["workspaces"][0]["cwd"] == str(desktop.project)
    root_history = desktop.profile / "data" / "scrollback" / (
        root_id.replace("-", "") + "-" + saved_root["root_leaf"].replace("-", "") + ".txt")
    durable = root_history.read_text()
    assert "TC_GUI_LARGE_START" in durable and "TC_GUI_LARGE_END" in durable
    assert len(durable.encode()) > 512 * 1024
    result["large_history_durable_bytes"] = len(durable.encode())

    desktop.start()
    desktop.screenshot("reopened-native-window")
    desktop.key("ctrl+shift+bracketleft")
    wait_for("focus restored root panel", lambda: desktop.root(root_id)["focused"])
    assert leaf_ids(desktop.root(root_id)["split_tree"]) == saved_leaves
    right = desktop.export("right-after-reopen")
    assert "TC_GUI_RIGHT_ONLY" in right and "TC_GUI_LARGE_START" not in right
    desktop.key("ctrl+alt+w")
    wait_for("close one split leaf", lambda: len(leaf_ids(desktop.root(root_id).get("split_tree"))) <= 1)
    assert_large_history(desktop.export("large-after-reopen"))
    result["native_gui_persistence_roundtrip"] = True

    old_layout = desktop.layout_path.read_bytes()
    (desktop.profile / "data").chmod(0o500)
    desktop.rename("GUI Blocked Save")
    time.sleep(2.5)
    wait_for("visible layout failure banner", lambda:
             "no se pudo guardar el layout" in desktop.ocr("save-failed"), timeout=15)
    assert desktop.layout_path.read_bytes() == old_layout
    (desktop.profile / "data").chmod(desktop.data_mode)
    desktop.rename("GUI Recovery")
    wait_for("current layout saved after recovery", lambda:
             desktop.root(root_id).get("custom_title") == "GUI Recovery")
    wait_for("failure banner cleared", lambda:
             "no se pudo guardar el layout" not in desktop.ocr("save-recovered"), timeout=15)
    result["native_gui_permission_failure_and_retry"] = True

    pid_file = desktop.project / "stream.pid"
    desktop.shell_command(f"python3 {shlex.quote(str(desktop.fixture))} {shlex.quote(str(pid_file))}")
    stream_pid = int(wait_for("active child process", lambda: pid_file.read_text() if pid_file.exists() else None))
    wait_for("stream persisted while app is active", lambda: any(
        b"TC_GUI_STREAM_000010" in p.read_bytes()
        for p in (desktop.profile / "data" / "scrollback").glob("*.mtlg")))
    desktop.screenshot("closing-with-active-output")
    result["active_output_close_seconds"] = desktop.close()
    assert "TC_GUI_STREAM_000010" in root_history.read_text()

    def child_exited():
        status = Path(f"/proc/{stream_pid}/status")
        return not status.exists() or "State:\tZ" in status.read_text()

    wait_for("foreground stream child physically exited", child_exited, timeout=10)
    result["native_gui_active_output_close"] = True
    result["provider_authenticated_flows"] = False
    result["online_two_physical_machines"] = False
    result["native_file_picker"] = False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if sys.flags.optimize or sys.platform != "linux" or os.environ.get("GITHUB_ACTIONS") != "true":
        parser.error("This test only runs on an isolated Linux GitHub Actions desktop")
    if not os.environ.get("DISPLAY") or not args.output.is_absolute():
        parser.error("DISPLAY and an absolute unused output directory are required")
    binary = args.binary.resolve(strict=True)
    args.output.mkdir()
    result = {"head": os.environ.get("GITHUB_SHA"), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "platform": "Linux X11 / WGPU software adapter", "completed": False}
    desktop = Desktop(binary, args.output)
    try:
        run(desktop, result)
        result["completed"] = True
    except BaseException as error:
        result["error"] = f"{type(error).__name__}: {error}"
        if desktop.window and desktop.process and desktop.process.poll() is None:
            desktop.screenshot("failure")
        raise
    finally:
        desktop.cleanup()
        write_new(args.output / "result.json", json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2), flush=True)


if __name__ == "__main__":
    main()
