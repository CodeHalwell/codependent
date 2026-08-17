import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check_migration_immutability.py"


class MigrationImmutabilityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        scripts = self.root / ".github" / "scripts"
        scripts.mkdir(parents=True)
        self.script = scripts / SCRIPT.name
        self.script.write_bytes(SCRIPT.read_bytes())
        self.migrations = self.root / "migrations"
        self.migrations.mkdir()
        (self.migrations / "0001_initial.sql").write_text(
            "CREATE TABLE example (id INTEGER PRIMARY KEY);\n",
            encoding="utf-8",
        )
        self.run_checker("--update", expected=0)

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def run_checker(self, *arguments: str, expected: int) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            [sys.executable, str(self.script), *arguments],
            cwd=self.root,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return result

    def test_rejects_a_changed_historical_migration(self) -> None:
        (self.migrations / "0001_initial.sql").write_text("SELECT 1;\n", encoding="utf-8")
        result = self.run_checker(expected=1)
        self.assertIn("historical migration changed", result.stderr)

    def test_rejects_a_deleted_historical_migration(self) -> None:
        (self.migrations / "0001_initial.sql").unlink()
        result = self.run_checker(expected=1)
        self.assertIn("historical migration was deleted", result.stderr)

    def test_allows_only_an_explicitly_recorded_append(self) -> None:
        (self.migrations / "0002_append.sql").write_text("SELECT 2;\n", encoding="utf-8")
        self.run_checker(expected=1)
        self.run_checker("--update", expected=0)
        self.run_checker(expected=0)

    def test_recognizes_sqlx_versions_longer_than_four_digits(self) -> None:
        (self.migrations / "10000_append.sql").write_text("SELECT 3;\n", encoding="utf-8")
        result = self.run_checker(expected=1)
        self.assertIn("10000_append.sql", result.stderr)

    def test_rejects_tampering_with_migration_and_manifest_together(self) -> None:
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "test@example.invalid"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "Migration Test"], cwd=self.root, check=True)
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "baseline"], cwd=self.root, check=True)
        (self.root / "marker").write_text("second commit\n", encoding="utf-8")
        subprocess.run(["git", "add", "marker"], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "head"], cwd=self.root, check=True)

        changed = b"SELECT 99;\n"
        (self.migrations / "0001_initial.sql").write_bytes(changed)
        manifest = {"0001_initial.sql": hashlib.sha384(changed).hexdigest()}
        (self.migrations / "checksums.json").write_text(
            json.dumps(manifest, indent=2) + "\n",
            encoding="utf-8",
        )
        result = self.run_checker(expected=1)
        self.assertIn("historical checksum entry changed", result.stderr)


if __name__ == "__main__":
    unittest.main()
