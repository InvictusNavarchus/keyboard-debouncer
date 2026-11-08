"""Exercise installation in a temporary filesystem, with no root or network.

Only absolute system paths are redirected in the script copies. Mock sudo uses
mock id to simulate elevation; all service/account/device commands are logged.
"""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-musl"
VERSION = "0.6.0"
PACKAGE = f"keyboard-debouncer-v{VERSION}-{TARGET}"
MOCK = r'''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ["COMMAND_LOG"], "a") as log:
    log.write(json.dumps([name, *args]) + "\n")
if name == "id":
    print("0" if len(args) > 1 or os.environ.get("MOCK_ROOT") else "1000")
elif name == "sudo":
    os.environ["MOCK_ROOT"] = "1"
    os.execvpe(args[0], args, os.environ)
elif name == "uname":
    print(os.environ.get("MOCK_OS", "Linux") if args == ["-s"] else os.environ.get("MOCK_ARCH", "x86_64"))
elif name == "systemctl":
    if args[0] in ("is-active", "is-enabled"):
        sys.exit(0 if os.environ.get("ACTIVE") == "1" else 3)
elif name == "curl":
    if "-w" in args:
        print("https://github.com/InvictusNavarchus/keyboard-debouncer/releases/tag/v0.6.0", end="")
    else:
        url = next(a for a in args if a.startswith("https://"))
        source = pathlib.Path(os.environ["RELEASE_DIR"]) / url.rsplit("/", 1)[1]
        if not source.is_file(): sys.exit(22)
        shutil.copyfile(source, args[args.index("-o") + 1])
elif name == "getent":
    print("kbd-debouncer:x:999:999:keyboard-debouncer daemon:/nonexistent:/usr/sbin/nologin")
elif name == "pgrep":
    sys.exit(1)
'''


class Installation(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="keyboard-install-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.system = self.root / "system"
        for name in ("etc/udev/rules.d", "usr/local/bin", "var/lib"):
            (self.system / name).mkdir(parents=True)
        paths = ["install.sh", "uninstall.sh", "keyboard-debouncer.service",
                 "debouncer.conf.example", "README.md", "CHANGELOG.md", "LICENSE",
                 "package.json", "scripts/analyze.ts", "scripts/timing.ts",
                 "scripts/report.sh", "scripts/package-release.sh"]
        for name in paths:
            dest = self.source / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            text = (REPO / name).read_text()
            if name in ("install.sh", "uninstall.sh", "scripts/report.sh"):
                for prefix in ("/usr/local", "/etc", "/var/lib"):
                    text = text.replace(prefix, str(self.system) + prefix)
            dest.write_text(text)
        shutil.copytree(REPO / "docs", self.source / "docs")
        binary = self.source / f"target/{TARGET}/release/keyboard-debouncer"
        binary.parent.mkdir(parents=True)
        binary.write_text(f"#!/bin/sh\necho keyboard-debouncer {VERSION}\n")
        binary.chmod(0o755)
        self.binary = binary
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("id", "sudo", "uname", "systemctl", "curl", "modprobe",
                     "useradd", "usermod", "userdel", "chown", "udevadm", "getent", "pgrep", "bun"):
            mock = self.bin / name
            mock.write_text(MOCK)
            mock.chmod(0o755)
        self.log = self.root / "commands.jsonl"
        self.env = dict(os.environ, PATH=str(self.bin) + ":" + os.environ["PATH"],
                        COMMAND_LOG=str(self.log), RELEASE_DIR=str(self.source / "dist"))
        self.env.pop("VERSION", None)
        self.env.pop("MOCK_ROOT", None)
        self.run_script("scripts/package-release.sh", VERSION, TARGET)

    def run_script(self, script="install.sh", *args, ok=True, env=None, directory=None):
        result = subprocess.run(["bash", str((directory or self.source) / script), *args],
                                cwd=self.root, env=env or self.env,
                                text=True, capture_output=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def commands(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def test_download_and_install_without_retaining_the_bundle(self):
        # Piping the bootstrap is supported: there is no local SCRIPT_DIR.
        result = subprocess.run(["bash"], input=(self.source / "install.sh").read_text(),
                                cwd=self.root, env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for name in ("keyboard-debouncer", "keyboard-debouncer-report", "keyboard-debouncer-uninstall"):
            self.assertTrue((self.system / "usr/local/bin" / name).is_file())
        self.assertTrue((self.system / "usr/local/share/keyboard-debouncer/scripts/timing.ts").is_file())
        self.assertTrue((self.system / "etc/debouncer.conf").is_file())
        self.assertFalse(any(c[0] == "systemctl" and c[1] in ("restart", "start") for c in self.commands()))
        sudo = next(c for c in self.commands() if c[0] == "sudo")
        self.assertFalse(Path(sudo[2]).exists(), "temporary download must be cleaned up")

    def test_update_preserves_config_and_data_and_restarts_active_service(self):
        config = self.system / "etc/debouncer.conf"
        config.write_text("my configuration")
        state = self.system / "var/lib/keyboard-debouncer"
        state.mkdir()
        (state / "keys.db").write_text("my history")
        self.run_script(env=dict(self.env, ACTIVE="1"))
        self.assertEqual(config.read_text(), "my configuration")
        self.assertEqual((state / "keys.db").read_text(), "my history")
        self.assertIn(["systemctl", "try-restart", "keyboard-debouncer"], self.commands())

    def test_bundled_binary_and_explicit_source_binary_work_offline(self):
        self.run_script("install.sh", "--binary", str(self.binary))
        self.assertFalse(any(c[0] == "curl" for c in self.commands()))
        shutil.copy2(self.binary, self.source / "keyboard-debouncer")
        self.run_script()
        self.assertFalse(any(c[0] == "curl" for c in self.commands()))

    def test_pinned_release_skips_latest_lookup(self):
        self.run_script(env=dict(self.env, VERSION="v" + VERSION))
        self.assertFalse(any(c[0] == "curl" and "-w" in c for c in self.commands()))

    def test_corrupt_or_missing_checksums_prevent_system_changes(self):
        archive = self.source / "dist" / (PACKAGE + ".tar.gz")
        archive.write_bytes(archive.read_bytes() + b"corruption")
        self.run_script(ok=False)
        checksum = archive.with_name(archive.name + ".sha256")
        checksum.unlink()
        self.run_script(ok=False)
        self.assertFalse(any(c[0] in ("sudo", "usermod", "useradd", "systemctl") for c in self.commands()))
        self.assertFalse((self.system / "usr/local/bin/keyboard-debouncer").exists())

    def test_invalid_arguments_and_unsupported_platforms_fail_before_download(self):
        self.run_script("install.sh", "--binary", "", ok=False)
        self.run_script("install.sh", "--binary", "/missing/binary", ok=False)
        self.run_script(env=dict(self.env, MOCK_ARCH="aarch64"), ok=False)
        self.run_script(env=dict(self.env, MOCK_OS="Darwin"), ok=False)
        self.assertFalse(any(c[0] == "curl" for c in self.commands()))

    def test_installed_uninstaller_removes_helpers_and_preserves_user_data(self):
        self.run_script()
        state = self.system / "var/lib/keyboard-debouncer"
        state.mkdir()
        (state / "keys.db").write_text("my history")
        self.run_script("usr/local/bin/keyboard-debouncer-uninstall",
                        directory=self.system, env=dict(self.env, MOCK_ROOT="1"))
        self.assertFalse((self.system / "usr/local/bin/keyboard-debouncer").exists())
        self.assertFalse((self.system / "usr/local/bin/keyboard-debouncer-report").exists())
        self.assertFalse((self.system / "usr/local/share/keyboard-debouncer").exists())
        self.assertTrue((self.system / "etc/debouncer.conf").exists())
        self.assertEqual((state / "keys.db").read_text(), "my history")

    def test_installed_report_forwards_arguments_using_the_callers_bun(self):
        self.run_script()
        self.run_script("usr/local/bin/keyboard-debouncer-report", "--db", "/a path/keys.db", "--details",
                        directory=self.system)
        self.assertIn(["bun", str(self.system / "usr/local/share/keyboard-debouncer/scripts/analyze.ts"),
                       "--db", "/a path/keys.db", "--details"], self.commands())

    def test_bundle_contains_only_runtime_files_even_on_repeated_packaging(self):
        staging = self.source / "dist" / PACKAGE
        (staging / "unwanted-source.rs").write_text("stale build output")
        self.run_script("scripts/package-release.sh", VERSION, TARGET)
        archive = self.source / "dist" / (PACKAGE + ".tar.gz")
        with tarfile.open(archive) as bundle:
            names = {n.removeprefix(PACKAGE + "/") for n in bundle.getnames()}
        self.assertIn("scripts/analyze.ts", names)
        self.assertIn("scripts/report.sh", names)
        for name in ("src", "Cargo.toml", "Cargo.lock", "tests", ".github", "release.toml",
                     "scripts/check-release.sh", "scripts/package-release.sh", "unwanted-source.rs"):
            self.assertNotIn(name, names)
        expected = archive.with_name(archive.name + ".sha256").read_text().split()[0]
        self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), expected)


if __name__ == "__main__":
    unittest.main()
