"""Regression checks for tag boundaries and publication guards; uses disposable Git repos."""
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("release-notes.py").resolve()
SECTIONS = ("Highlights", "Fixes", "Compatibility and update notes", "Known limitations")


class ReleaseNotesTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Release test")
        self.git("config", "user.email", "release-test@example.invalid")
        self.git("commit", "--allow-empty", "-qm", "Previous release")
        self.git("tag", "v0.1.0")
        (self.root / "docs/releases").mkdir(parents=True)
        self.notes = self.root / "docs/releases/v0.2.0.md"
        self.notes.write_text("\n\n".join(f"## {s}\n\nReviewed change." for s in SECTIONS), encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "Reviewed release notes")
        self.git("tag", "v0.2.0")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, stderr=subprocess.STDOUT)

    def run_script(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), *args], cwd=self.root, capture_output=True, text=True, encoding="utf-8")

    def test_tagged_notes_exclude_newer_master_and_preserve_packages(self):
        self.notes.write_text("NEWER MASTER CONTENT", encoding="utf-8")
        self.git("commit", "-qam", "Post-release changes")
        (self.root / "packages.md").write_text("Signature verified.\nSHA256 abc123", encoding="utf-8")
        result = self.run_script("--tag", "v0.2.0", "--package-notes", "packages.md", "--output", "result.md")
        self.assertEqual(result.returncode, 0, result.stderr)
        output = (self.root / "result.md").read_text(encoding="utf-8")
        self.assertIn("Reviewed change.", output)
        self.assertNotIn("NEWER MASTER", output)
        self.assertIn("compare/v0.1.0...v0.2.0", output)
        self.assertIn("## Downloads\n\nSignature verified.\nSHA256 abc123", output)

    def test_draft_and_missing_notes_cannot_be_published(self):
        self.notes.write_text("TODO UNRELEASED", encoding="utf-8")
        self.git("commit", "-qam", "Incomplete notes")
        self.git("tag", "v0.3.0")
        self.assertNotEqual(self.run_script("--tag", "v0.3.0", "--output", "result.md").returncode, 0)
        (self.root / "docs/releases/v0.4.0.md").write_text("TODO UNRELEASED", encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "Draft notes")
        self.git("tag", "v0.4.0")
        self.assertNotEqual(self.run_script("--tag", "v0.4.0", "--output", "result.md").returncode, 0)
        self.assertFalse((self.root / "result.md").exists())

    def test_generated_draft_is_explicitly_incomplete(self):
        self.git("commit", "--allow-empty", "-qm", "Unreleased fix")
        result = self.run_script("--draft", "--tag", "v0.3.0", "--output", "draft.md")
        self.assertEqual(result.returncode, 0, result.stderr)
        draft = (self.root / "draft.md").read_text(encoding="utf-8")
        self.assertIn("TODO", draft)
        self.assertIn("Unreleased fix", draft)


if __name__ == "__main__":
    unittest.main()
