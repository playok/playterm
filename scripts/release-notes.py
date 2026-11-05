"""Compose reviewed, tag-scoped user notes with generated package/signature information."""
import argparse
import re
import subprocess
from pathlib import Path

SECTIONS = ("Highlights", "Fixes", "Compatibility and update notes", "Known limitations")


def git(*args):
    return subprocess.check_output(["git", *args], text=True, encoding="utf-8").strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repository", default="playok/playterm")
    parser.add_argument("--package-notes", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--draft", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"v\d+\.\d+\.\d+", args.tag):
        parser.error("tag must be vMAJOR.MINOR.PATCH")
    if not re.fullmatch(r"[\w.-]+/[\w.-]+", args.repository):
        parser.error("invalid repository")
    if args.draft:
        previous = git("describe", "--tags", "--abbrev=0", "HEAD")
        commits = git("log", "--no-merges", "--format=- %s (%h)", f"{previous}..HEAD")
        notes = "\n\n".join(f"## {section}\n\nTODO: review before tagging." for section in SECTIONS)
        args.output.write_text(f"# {args.tag} draft\n\n{notes}\n\n## Commit candidates\n\n{commits}\n", encoding="utf-8")
        return
    notes = git("show", f"{args.tag}:docs/releases/{args.tag}.md")
    if "TODO" in notes or "UNRELEASED" in notes or any(f"## {section}" not in notes for section in SECTIONS):
        parser.error("tagged release notes need all four reviewed sections and no TODO/UNRELEASED markers")
    previous = git("describe", "--tags", "--abbrev=0", f"{args.tag}^")
    compare = f"https://github.com/{args.repository}/compare/{previous}...{args.tag}"
    packages = args.package_notes.read_text(encoding="utf-8") if args.package_notes else ""
    args.output.write_text(f"{notes}\n\n[All changes since {previous}]({compare})\n\n## Downloads\n\n{packages}", encoding="utf-8")


if __name__ == "__main__":
    main()
