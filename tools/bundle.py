"""Assembles the native mod as an unpacked .hnmod, the layout the launcher
reads: horizon-native.json, and mixins/horizon_voice/ holding mod/ (manifest,
helpers, glue module) and engine/ (the mod's crate).

    python tools/bundle.py OUT_DIR       -> OUT_DIR/horizon_voice/

horizon-native.json pins horizon-mixins 0.6.0 (a2af5c0): the engine crate
([[crate]]) and named hooks need it, and the launcher builds a set of mods with
the newest horizon-mixins any of them pins.
"""

import json
import pathlib
import re
import shutil
import sys

MIXINS = "a2af5c0dd4117d51ecd70066ba7a00b4a2c10dab"

root = pathlib.Path(__file__).resolve().parent.parent
out = pathlib.Path(sys.argv[1]).resolve() / "horizon_voice"
mixins = out / "mixins" / "horizon_voice"

manifest = (root / "mod" / "horizon-mixin.toml").read_text(encoding="utf-8")
version = re.search(r'^version = "([^"]+)"', manifest, re.M).group(1)
horizon = re.search(r'^horizon_revision = "([0-9a-f]{40})"', manifest, re.M).group(1)

if out.exists():
    shutil.rmtree(out)
shutil.copytree(root / "mod", mixins)
shutil.copytree(
    root / "engine",
    mixins / "engine",
    ignore=shutil.ignore_patterns("target", "Cargo.lock", ".git"),
)
(out / "horizon-native.json").write_text(
    json.dumps(
        {
            "game": "26.3",
            "horizon": horizon,
            "id": "horizon_voice",
            "mixins": MIXINS,
            "schema": 1,
            "version": version,
        },
        indent=2,
    )
    + "\n",
    encoding="utf-8",
)
print(out)
