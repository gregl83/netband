"""Exercise benchmark provenance without contacting a measurement server."""
import csv
import hashlib
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


class BenchmarkTests(unittest.TestCase):
    def test_metadata_and_commands_describe_the_executed_binaries(self):
        self.run_benchmark()

    def test_failed_measurements_remain_recorded(self):
        self.run_benchmark(client_exit=1)

    def test_failed_version_check_stops_before_measurements(self):
        self.run_benchmark(version_exit=1)

    def run_benchmark(self, client_exit=0, version_exit=0):
        script = Path(__file__).resolve().with_name('benchmark-ndt7-clients.sh')
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            bin_dir = root / 'fake bin'
            bin_dir.mkdir()
            programs = {
                'getent': 'echo "192.0.2.1 STREAM fixture"',
                'ip': 'echo "192.0.2.1 dev fixture0 src 192.0.2.2"',
                'reference': '''echo '{"ServerFQDN":"fixture","Download":{"Throughput":{"Value":100}},"Upload":{"Throughput":{"Value":50}}}' ''',
                'netband': '''case "$*" in
                  *--version*) echo 'netband 1.0.0'; exit "$TEST_VERSION_EXIT" ;;
                  *'config check'*) echo 'configuration=valid'; exit 0 ;;
                esac
                [ "$TEST_CLIENT_EXIT" = 0 ] || exit "$TEST_CLIENT_EXIT"
                while [ "$#" -gt 0 ]; do
                  if [ "$1" = --output ]; then
                    printf 'event_kind,outcome,download_mbps,upload_mbps\\nbandwidth,success,100,50\\n' >"$2"
                    break
                  fi
                  shift
                done''',
            }
            for name, body in programs.items():
                path = bin_dir / name
                path.write_text('#!/bin/sh\n' + body + ('\nexit "$TEST_CLIENT_EXIT"\n' if name in ['netband', 'reference'] else '\n'))
                path.chmod(0o755)
            output = root / 'results'
            env = dict(os.environ, PATH=f'{bin_dir}:{os.environ["PATH"]}',
                       NETBAND_BIN=str(bin_dir / 'netband'), NDT7_CLIENT_BIN=str(bin_dir / 'reference'),
                       NETBAND_BENCHMARK_NOTES='router=new; ap=office; wifi=5 GHz',
                       TEST_CLIENT_EXIT=str(client_exit), TEST_VERSION_EXIT=str(version_exit))
            result = subprocess.run(['bash', str(script), 'fixture.invalid', '1', '0', str(output)],
                                    env=env, capture_output=True, text=True)
            if version_exit:
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(output.exists())
                return
            self.assertEqual(result.returncode, 0, result.stderr)
            with (output / 'measurements.csv').open() as stream:
                rows = list(csv.DictReader(stream))
            self.assertEqual(len(rows), 2)
            self.assertTrue(all(int(row['exit_code']) == client_exit for row in rows))
            metadata = (output / 'metadata.txt').read_text()
            for name, key in [('netband', 'netband_sha256'), ('reference', 'ndt7_client_sha256')]:
                digest = hashlib.sha256((bin_dir / name).read_bytes()).hexdigest()
                self.assertIn(f'{key}={digest}', metadata)
            self.assertIn('netband_version=netband 1.0.0', metadata)
            self.assertIn('pairs=1\ncooldown_seconds=0\n', metadata)
            note = next(line.split('=', 1)[1] for line in metadata.splitlines() if line.startswith('network_notes='))
            self.assertEqual(shlex.split(note), [env['NETBAND_BENCHMARK_NOTES']])
            self.assertIn('configuration=valid', (output / 'netband-config.txt').read_text())
            command = shlex.split((output / 'raw/pair-01-first-netband.command').read_text())
            self.assertEqual(command[0], str(bin_dir / 'netband'))
            self.assertEqual(command[-2:], ['once', 'bandwidth'])
            self.assertIn('--force', command)
            self.assertEqual(command[command.index('--ndt-target') + 1], 'fixture.invalid')
            self.assertEqual(command[command.index('--output') + 1], str(output / 'raw/pair-01-first-netband.csv'))
            reference = shlex.split((output / 'raw/pair-01-second-reference.command').read_text())
            self.assertEqual(reference, [str(bin_dir / 'reference'), '-server', 'fixture.invalid', '-format', 'json', '-quiet'])


if __name__ == '__main__':
    unittest.main()
