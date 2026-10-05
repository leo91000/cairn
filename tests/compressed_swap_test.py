"""Both host installers enable compressed swap so memory peaks slow VMs down instead of stalling them."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
INSTALLERS = [REPO / 'deploy/nodes/install.sh', REPO / 'deploy/installations/install.sh']
START = '# >>> compressed swap'
END = '# <<< compressed swap'


def function(installer):
    script = installer.read_text()
    return script[script.index(START):script.index(END) + len(END)]


class CompressedSwap(unittest.TestCase):
    def run_function(self, root, swaps, with_apt=True):
        binaries = root / 'bin'
        binaries.mkdir()
        log = root / 'calls'
        commands = ['systemctl', 'sysctl'] + (['apt-get'] if with_apt else [])
        for command in commands:
            path = binaries / command
            path.write_text(f'#!/bin/sh\necho "{command} $*" >> "{log}"\n')
            path.chmod(0o755)
        # Only the tools the function needs; never the host's real apt-get.
        (binaries / 'grep').symlink_to(shutil.which('grep'))
        (root / 'swaps').write_text(swaps)
        (root / 'etc/default').mkdir(parents=True)
        (root / 'etc/sysctl.d').mkdir()
        script = function(INSTALLERS[0]) + '\nensure_compressed_swap\n'
        result = subprocess.run(
            [shutil.which('bash'), '-c', script],
            env={'PATH': str(binaries), 'LEO_PROC_SWAPS': str(root / 'swaps'),
                 'LEO_ETC_DIR': str(root / 'etc')},
            capture_output=True, text=True)
        calls = log.read_text() if log.exists() else ''
        return result, calls

    def test_both_installers_share_the_same_function(self):
        self.assertEqual(function(INSTALLERS[0]), function(INSTALLERS[1]))
        for installer in INSTALLERS:
            self.assertRegex(installer.read_text(), r'(?m)^\s*ensure_compressed_swap$', installer)

    def test_a_host_without_zram_gets_zstd_compressed_swap(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, calls = self.run_function(root, 'Filename Type Size Used Priority\n/dev/sda2 partition 524284 0 -2\n')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('apt-get install -y -qq zram-tools', calls)
            self.assertIn('systemctl restart zramswap', calls)
            self.assertEqual((root / 'etc/default/zramswap').read_text(), 'ALGO=zstd\nPERCENT=25\nPRIORITY=100\n')
            self.assertEqual((root / 'etc/sysctl.d/99-leo-zram.conf').read_text(), 'vm.swappiness=100\n')

    def test_an_existing_zram_swap_is_left_untouched(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, calls = self.run_function(root, 'Filename Type Size Used Priority\n/dev/zram0 partition 16777212 0 100\n')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(calls, '')
            self.assertFalse((root / 'etc/default/zramswap').exists())

    def test_hosts_without_apt_keep_installing_with_a_warning(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, calls = self.run_function(root, 'Filename Type Size Used Priority\n', with_apt=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('enable zram compressed swap', result.stderr)
            self.assertEqual(calls, '')


if __name__ == '__main__':
    unittest.main()
