"""Packs the mod into horizon_voice.hnmod, the file the launcher installs:
a zip of horizon-native.json and mixins/horizon_voice/ (see tools/bundle.py),
holding only what the registry's check accepts (Rust and TOML).

    python tools/package.py OUT_DIR      -> OUT_DIR/horizon_voice.hnmod
"""

import pathlib
import subprocess
import sys
import tempfile
import zipfile

root = pathlib.Path(__file__).resolve().parent
out_dir = pathlib.Path(sys.argv[1]).resolve()
out_dir.mkdir(parents=True, exist_ok=True)


def allowed(rest: str) -> bool:
    # The registry's check takes these alone, whatever newer launchers accept.
    return rest.endswith(".rs") or rest.endswith(".toml")


with tempfile.TemporaryDirectory() as tmp:
    subprocess.run([sys.executable, "-I", str(root / "bundle.py"), tmp], check=True, stdout=subprocess.DEVNULL)
    bundle = pathlib.Path(tmp) / "horizon_voice"
    target = out_dir / "horizon_voice.hnmod"
    prefix = "mixins/horizon_voice/"
    with zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(bundle / "horizon-native.json", "horizon-native.json")
        for path in sorted((bundle / "mixins" / "horizon_voice").rglob("*")):
            if path.is_dir():
                continue
            rest = path.relative_to(bundle / "mixins" / "horizon_voice").as_posix()
            if not allowed(rest):
                sys.exit(f"a .hnmod can't carry {rest}")
            z.write(path, prefix + rest)
    print(target)
