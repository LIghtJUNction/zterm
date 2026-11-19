"""Check the published crate boundary without compiling or publishing it."""
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[1]
probe = root / "artifacts/package-boundary-fixture/nested"
probe.mkdir(parents=True, exist_ok=True)
for name in ("README.md", "LICENSE"):
    (probe / name).write_text("This ignored probe must not enter the crate.\n")

paths = {
    path.replace('\\', "/")
    for path in subprocess.check_output(
        ["cargo", "package", "--locked", "--list"], cwd=root, text=True
    ).splitlines()
}
required = {"Cargo.toml", "Cargo.lock", "README.md", "LICENSE"}
required.update(path.relative_to(root).as_posix() for path in (root / "src").glob("**/*.rs"))
required.update(path.relative_to(root).as_posix() for path in (root / "examples").glob("**/*.rs"))
required.update(
    path.relative_to(root).as_posix()
    for path in (root / "tests/data").rglob("*")
    if path.is_file()
)
missing = required - paths
extra = {
    path
    for path in paths
    if path not in required | {"Cargo.toml.orig", ".cargo_vcs_info.json"}
}
if missing or extra:
    raise SystemExit(f"Package boundary failed: missing={sorted(missing)}, extra={sorted(extra)}")
print(f"Package boundary passed: {len(paths)} files, all Rust modules and fixtures included.")
