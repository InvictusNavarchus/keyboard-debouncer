"""Exercise system installation/removal in redirected paths with mocked administration."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
MOCK = '''#!/usr/bin/env python3
import json, os, pathlib, sys
name, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
with open(os.environ['COMMAND_LOG'], 'a') as f: f.write(json.dumps([name, *args])+'\\n')
if name == 'pgrep': sys.exit(1)
if name == 'systemctl':
    if os.environ.get('FAIL_ACTION') == args[0]: sys.exit(1)
    if args[0] == 'is-active': sys.exit(0 if os.environ.get('ACTIVE') else 3)
    if args[0] == 'is-enabled': sys.exit(1)
'''


class ServiceInstallation(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='debouncer-service-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.checkout = self.root / 'checkout'
        self.checkout.mkdir()
        self.host = self.root / 'host'
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        self.log = self.root / 'commands.jsonl'
        for name in ['id', 'systemctl', 'modprobe', 'useradd', 'usermod', 'userdel', 'udevadm', 'chown', 'pgrep', 'loginctl', 'sudo']:
            p = self.tools / name
            p.write_text(MOCK)
            p.chmod(0o755)
        for name in ['install.sh', 'uninstall.sh']:
            text = (REPO / name).read_text()
            for prefix in ['/usr/local/bin', '/usr/local/share', '/etc', '/var/lib']:
                text = text.replace(prefix, str(self.host) + prefix)
            text = text.replace('${EUID:-$(id -u)}', '0').replace('"$(id -u)"', '"0"')
            (self.checkout / name).write_text(text)
        for name in ['keyboard-debouncer.service', 'debouncer.conf.example']:
            shutil.copyfile(REPO / name, self.checkout / name)
        binary = self.checkout / 'binary'
        binary.write_text('#!/bin/sh\nexit 0\n')
        binary.chmod(0o755)
        self.binary = binary
        self.unit = self.host / 'etc/systemd/system/keyboard-debouncer.service'
        self.unit.parent.mkdir(parents=True)
        self.env = dict(os.environ, PATH=str(self.tools)+':'+os.environ['PATH'], COMMAND_LOG=str(self.log))

    def commands(self):
        return [json.loads(x) for x in self.log.read_text().splitlines()]

    def install(self, *, ok=True, **env):
        p = subprocess.run(['bash', str(self.checkout / 'install.sh'), '--binary', str(self.binary)],
                           env=dict(self.env, **env), capture_output=True, text=True)
        self.assertEqual(p.returncode == 0, ok, p.stdout+p.stderr)
        return p

    def test_fresh_install_stays_inactive_and_preserves_configuration(self):
        config = self.host / 'etc/debouncer.conf'
        config.write_text('custom config')
        self.install()
        self.assertEqual(config.read_text(), 'custom config')
        actions = [c[1] for c in self.commands() if c[0] == 'systemctl']
        self.assertNotIn('enable', actions)
        self.assertNotIn('start', actions)
        self.assertNotIn('try-restart', actions)

    def test_active_update_restarts_without_enabling_and_keeps_dropins(self):
        override = self.unit.parent / 'keyboard-debouncer.service.d/override.conf'
        override.parent.mkdir()
        override.write_text('[Service]\nNice=10\n')
        self.install(ACTIVE='1')
        self.assertIn(['systemctl', 'try-restart', 'keyboard-debouncer'], self.commands())
        self.assertFalse(any(c[:2] == ['systemctl', 'enable'] for c in self.commands()))
        self.assertEqual(override.read_text(), '[Service]\nNice=10\n')

    def test_mask_is_preserved_and_foreign_unit_symlink_is_rejected(self):
        self.unit.symlink_to('/dev/null')
        self.install()
        self.assertEqual(os.readlink(self.unit), '/dev/null')
        self.unit.unlink()
        foreign = self.root / 'foreign.service'
        foreign.write_text('operator unit')
        self.unit.symlink_to(foreign)
        installed = self.host / 'usr/local/bin/keyboard-debouncer'
        before = installed.read_bytes()
        self.install(ok=False)
        self.assertEqual(installed.read_bytes(), before)
        self.assertEqual(foreign.read_text(), 'operator unit')

    def test_uninstall_preserves_account_data_and_lingering(self):
        self.install()
        state = self.host / 'var/lib/keyboard-debouncer'
        state.mkdir(parents=True)
        (state / 'keys.db').write_text('history')
        p = subprocess.run(['bash', str(self.checkout / 'uninstall.sh')],
                           env=self.env, capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stdout+p.stderr)
        self.assertFalse((self.host / 'usr/local/bin/keyboard-debouncer').exists())
        self.assertEqual((state / 'keys.db').read_text(), 'history')
        self.assertFalse(any(c[0] in ['userdel', 'loginctl'] for c in self.commands()))
        self.assertTrue((self.host / 'etc/debouncer.conf').exists())

    def test_purge_removes_data_but_retains_shared_account_and_module_entry(self):
        self.install()
        state = self.host / 'var/lib/keyboard-debouncer'
        state.mkdir(parents=True)
        (state / 'keys.db').write_text('history')
        p = subprocess.run(['bash', str(self.checkout / 'uninstall.sh'), '--purge'],
                           env=self.env, input='y\n', capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stdout+p.stderr)
        self.assertFalse(state.exists())
        self.assertFalse((self.host / 'etc/debouncer.conf').exists())
        self.assertTrue((self.host / 'etc/modules-load.d/uinput.conf').exists())
        self.assertFalse(any(c[0] in ['userdel', 'loginctl'] for c in self.commands()))

    def test_failed_staging_preserves_the_installed_binary(self):
        self.install()
        installed = self.host / 'usr/local/bin/keyboard-debouncer'
        before = installed.stat().st_ino
        tool = self.tools / 'install'
        tool.write_text('#!/bin/sh\n[ "$1" != "-m" ] || exit 1\nexec /usr/bin/install "$@"\n')
        tool.chmod(0o755)
        self.install(ok=False)
        self.assertEqual(installed.stat().st_ino, before)
        self.assertFalse(list(installed.parent.glob('.keyboard-debouncer-install.*')))

    def test_failed_stop_preserves_installed_binary(self):
        self.install()
        p = subprocess.run(['bash', str(self.checkout / 'uninstall.sh')],
                           env=dict(self.env, ACTIVE='1', FAIL_ACTION='stop'),
                           capture_output=True, text=True)
        self.assertNotEqual(p.returncode, 0)
        self.assertTrue((self.host / 'usr/local/bin/keyboard-debouncer').exists())

    def test_atomic_replacement_keeps_old_inode_and_restart_failure_is_reported(self):
        self.install()
        installed = self.host / 'usr/local/bin/keyboard-debouncer'
        previous = self.root / 'previous'
        os.link(installed, previous)
        self.binary.write_text('#!/bin/sh\necho updated\n')
        result = self.install(ACTIVE='1', FAIL_ACTION='try-restart', ok=False)
        self.assertNotEqual(installed.stat().st_ino, previous.stat().st_ino)
        self.assertEqual(previous.read_text(), '#!/bin/sh\nexit 0\n')
        self.assertNotIn('Upgrade successful', result.stdout)
        self.assertFalse(list(installed.parent.glob('.keyboard-debouncer-install.*')))


if __name__ == '__main__':
    unittest.main()
