from __future__ import annotations

import contextlib
import io
import json
import subprocess
import sys
import textwrap
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import contract_snapshot

ROOT = Path(__file__).resolve().parents[1]


class FrozenSnapshotTests(unittest.TestCase):
    def test_check_defaults_to_locked_artifact_without_sibling_source(self) -> None:
        lock = json.loads((ROOT / "contract.lock.json").read_text())
        with patch.object(contract_snapshot, "check_lock", return_value={}) as check:
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(contract_snapshot.main(["--repo-root", str(ROOT), "check"]), 0)
        check.assert_called_once_with(ROOT, "contract.lock.json", source=None, artifact=lock["artifact_url"])

    def test_frozen_baseline_is_independent_of_new_python_adoption(self) -> None:
        lock = json.loads((ROOT / "contract.lock.json").read_text())
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lock_path = root / "contract.lock.json"
            lock_path.write_text(json.dumps(lock))
            matrix_path = root / "support-matrix.json"
            matrix = {
                "schema_version": 2, "contract_version": "24.0.0", "status": "pending-adoption",
                "required_implementations": ["python"],
                "implementations": {
                    "python": {"contract_version": "24.0.0", "status": "pending-adoption"},
                    "rust": {
                        "contract_version": "23.0.0", "status": "frozen", "package_series": "0.21.x",
                        "verified_revision": "00f4240786f1adea1dc0c4730da8ddd06a5ab8ac",
                    },
                },
            }
            matrix_path.write_text(json.dumps(matrix))
            report = contract_snapshot.verify_adoption(
                root, "contract.lock.json", "rust", str(matrix_path),
            )
            self.assertEqual((report["contract_version"], report["status"]), ("23.0.0", "frozen"))
            self.assertIsNone(report["cross_repository_run"])
            lock["contract_version"] = "24.0.0"
            lock_path.write_text(json.dumps(lock))
            with self.assertRaisesRegex(contract_snapshot.SnapshotError, "pinned version"):
                contract_snapshot.verify_adoption(root, "contract.lock.json", "rust", str(matrix_path))
            lock["contract_version"] = "23.0.0"
            lock_path.write_text(json.dumps(lock))
            for field, invalid in (("package_series", None), ("verified_revision", "00f4240")):
                with self.subTest(field=field):
                    original = matrix["implementations"]["rust"][field]
                    matrix["implementations"]["rust"][field] = invalid
                    matrix_path.write_text(json.dumps(matrix))
                    with self.assertRaises(contract_snapshot.SnapshotError):
                        contract_snapshot.verify_adoption(root, "contract.lock.json", "rust", str(matrix_path))
                    matrix["implementations"]["rust"][field] = original
            matrix["schema_version"] = 1
            matrix_path.write_text(json.dumps(matrix))
            with self.assertRaisesRegex(contract_snapshot.SnapshotError, "schema_version=2"):
                contract_snapshot.verify_adoption(root, "contract.lock.json", "rust", str(matrix_path))

    def test_frozen_release_requires_verified_baseline_ancestry(self) -> None:
        matrix = {
            "schema_version": 2, "contract_version": "24.0.0", "status": "in-progress",
            "required_implementations": ["python"],
            "implementations": {
                "python": {},
                "rust": {
                    "contract_version": "23.0.0", "status": "frozen", "package_series": "0.21.x",
                    "verified_revision": "00f4240786f1adea1dc0c4730da8ddd06a5ab8ac",
                },
            },
        }
        with patch.object(contract_snapshot, "load_json_location", return_value=matrix):
            with self.assertRaisesRegex(contract_snapshot.SnapshotError, "does not contain"):
                contract_snapshot.verify_adoption(ROOT, "contract.lock.json", "rust", "matrix", "a" * 40)

    def test_publish_guard_allows_only_maintenance_series(self) -> None:
        workflow = (ROOT / ".github/workflows/publish-crate.yml").read_text()
        guard = textwrap.dedent(workflow.split("<<'PY'\n", 1)[1].split("          PY\n", 1)[0])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = root / "crates/vv-agent/Cargo.toml"
            manifest.parent.mkdir(parents=True)
            for version, allowed in (("0.21.2", True), ("0.21.99", True), ("0.22.0", False), ("1.0.0", False)):
                with self.subTest(version=version):
                    manifest.write_text(f'[package]\nversion = "{version}"\n')
                    result = subprocess.run([sys.executable, "-c", guard], cwd=root, capture_output=True)
                    self.assertEqual(result.returncode == 0, allowed, result.stderr)

    def test_artifact_digest_is_checked_before_extraction(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            artifact = Path(temporary) / "wrong.zip"
            artifact.write_bytes(b"not the locked release")
            with self.assertRaisesRegex(contract_snapshot.SnapshotError, "artifact digest mismatch"):
                contract_snapshot.check_lock(ROOT, "contract.lock.json", artifact=str(artifact))


if __name__ == "__main__":
    unittest.main()
