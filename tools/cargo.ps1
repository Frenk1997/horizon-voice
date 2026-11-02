# Runs cargo in engine/ with the Horizon launcher's own toolchain (Rust 1.99.1
# windows-gnu + MinGW-w64), the one it builds native mods with.
# The build directory is short: the project path is past Windows' MAX_PATH.
#   tools\cargo.ps1 test
$rust = "$env:APPDATA\horizon-launcher\rust\1.99.1-x86_64-pc-windows-gnu"
$env:PATH = "$rust\bin;$rust\mingw64\bin;$env:PATH"
$env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\Temp\hvt"
# The crate can't carry a [profile]; opus-rs needs optimizing to test in real time.
$env:CARGO_PROFILE_DEV_OPT_LEVEL = "2"
$env:CARGO_PROFILE_TEST_OPT_LEVEL = "2"
Push-Location (Join-Path $PSScriptRoot "..\engine")
try { cargo @args } finally { Pop-Location }
