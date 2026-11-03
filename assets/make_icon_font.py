"""Builds assets/icons/material-symbols-subset.ttf: only the Material Symbols icons that
crates/playterm/src/icon.rs uses, cut from the font of the egui_material_icons crate
(about 1.2 MB) down to a few kilobytes.

Run it again after adding an icon to icon.rs (the `icons_are_in_the_font` test fails until
then). Needs fontTools (`pip install fonttools`):

    python assets/make_icon_font.py [path/to/MaterialSymbolsRounded-Regular.ttf]

Without a path, the font is looked up in the Cargo registry (~/.cargo/registry/src), in the
egui_material_icons version that Cargo.lock names.
"""
import glob
import os
import re
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent.parent
ICON_RS = ROOT / "crates" / "playterm" / "src" / "icon.rs"
OUT = ROOT / "assets" / "icons" / "material-symbols-subset.ttf"
SOURCE_NAME = "MaterialSymbolsRounded-Regular.ttf"


def crate_dir():
    lock = (ROOT / "Cargo.lock").read_text(encoding="utf-8")
    m = re.search(r'name = "egui_material_icons"\nversion = "([^"]+)"', lock)
    if not m:
        sys.exit("egui_material_icons is not in Cargo.lock")
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    dirs = glob.glob(os.path.join(home, "registry", "src", "*", f"egui_material_icons-{m.group(1)}"))
    if not dirs:
        sys.exit(f"egui_material_icons {m.group(1)} is not in the Cargo registry: run `cargo build` first")
    return Path(dirs[0])


def used_codepoints(crate):
    """The code points of the `ICON_*` constants icon.rs uses."""
    names = set(re.findall(r"\b(ICON_\w+)\b", ICON_RS.read_text(encoding="utf-8")))
    table = (crate / "src" / "icons.rs").read_text(encoding="utf-8")
    points = {}
    for name in sorted(names):
        m = re.search(rf'pub const {name}: [^=]+= [^"]*"\\u\{{([0-9a-fA-F]+)\}}"', table)
        if not m:
            sys.exit(f"{name} is not in {crate / 'src' / 'icons.rs'}")
        points[name] = int(m.group(1), 16)
    return points


def main():
    crate = crate_dir()
    source = Path(sys.argv[1]) if len(sys.argv) > 1 else crate / SOURCE_NAME
    points = used_codepoints(crate)
    font = TTFont(source)
    options = subset.Options()
    options.layout_features = []  # no ligatures: icons are drawn by code point
    options.name_IDs = ["*"]  # keep the copyright and font names
    options.notdef_outline = True
    sub = subset.Subsetter(options)
    sub.populate(unicodes=sorted(set(points.values())))
    sub.subset(font)
    cmap = font.getBestCmap()
    missing = [n for n, p in points.items() if p not in cmap]
    if missing:
        sys.exit(f"not in the font: {', '.join(missing)}")
    OUT.parent.mkdir(parents=True, exist_ok=True)
    font.save(OUT)
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes, {len(set(points.values()))} icons) from {source}")


if __name__ == "__main__":
    main()
