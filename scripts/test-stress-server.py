"""Regression checks for atomic stress-server snapshots and Windows sharing conflicts."""
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

spec = importlib.util.spec_from_file_location("stress_server", Path(__file__).with_name("stress-server.py"))
server = importlib.util.module_from_spec(spec)
spec.loader.exec_module(server)


class SnapshotTests(unittest.TestCase):
    def test_transient_windows_reader_conflict_is_retried(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            server.write_snapshot(directory, {"active": 1})
            original = Path.replace
            attempts = []

            def replace(path, target):
                attempts.append(1)
                self.assertEqual(json.loads((directory / "server-stats.json").read_text())["active"], 1)
                if len(attempts) < 3:
                    raise PermissionError("simulated Windows reader sharing conflict")
                return original(path, target)

            with mock.patch.object(Path, "replace", autospec=True, side_effect=replace), mock.patch.object(server.time, "sleep"):
                server.write_snapshot(directory, {"active": 30})
            self.assertEqual(len(attempts), 3)
            snapshot = json.loads((directory / "server-stats.json").read_text())
            self.assertEqual(snapshot["active"], 30)
            self.assertIn("updated_unix", snapshot)
            self.assertFalse((directory / "server-stats.tmp").exists())

    def test_permanent_failure_is_bounded_and_preserves_previous_snapshot(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            server.write_snapshot(directory, {"active": 1})
            before = (directory / "server-stats.json").read_bytes()
            with mock.patch.object(Path, "replace", side_effect=PermissionError("permanent failure")) as replace, mock.patch.object(server.time, "sleep"):
                with self.assertRaises(PermissionError):
                    server.write_snapshot(directory, {"active": 30})
                self.assertEqual(replace.call_count, 40)
            self.assertEqual((directory / "server-stats.json").read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
