"""Vendor opus-rs into engine/src/opus as a module of the engine crate.

Horizon's native mods may only depend on crates already in Horizon's
Cargo.lock, and opus-rs isn't there; it has no required dependencies, so its
source goes in as a module instead. Run again to move to another version:

    python tools/vendor_opus.py PATH/TO/opus-rs-X.Y.Z
"""

import pathlib
import re
import shutil
import sys

src_root = pathlib.Path(sys.argv[1])
dest = pathlib.Path(__file__).resolve().parent.parent / "engine" / "src" / "opus"

version = re.search(r'^version = "([^"]+)"', (src_root / "Cargo.toml").read_text(), re.M).group(1)

if dest.exists():
    shutil.rmtree(dest)
shutil.copytree(src_root / "src", dest)
# A .hnmod carries license files only at the top of a crate folder.
shutil.copy(src_root / "COPYING", dest.parent.parent / "COPYING-opus-rs")

(dest / "lib.rs").rename(dest / "mod.rs")

# `#[macro_export]` macros stay at the crate root wherever they are defined.
macros = set()
for path in dest.rglob("*.rs"):
    macros |= set(re.findall(r"#\[macro_export\]\s*macro_rules!\s*(\w+)", path.read_text(encoding="utf-8")))

for path in dest.rglob("*.rs"):
    text = path.read_text(encoding="utf-8")
    text = re.sub(r"\bcrate::(?!(?:%s)\b)" % "|".join(macros or ["$^"]), "crate::opus::", text)
    if path.name == "mod.rs" and path.parent == dest:
        # Crate-level only; the engine crate is std.
        text = text.replace('#![cfg_attr(not(feature = "std"), no_std)]\n', "")
        text = f"// Vendored from opus-rs {version} (BSD-3-Clause, see engine/COPYING-opus-rs) by tools/vendor_opus.py.\n" + text
    path.write_text(text, encoding="utf-8")

print(f"vendored opus-rs {version} into {dest}")
