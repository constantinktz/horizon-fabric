#!/usr/bin/env python3
"""Checks for `fabric-mods/` (Horizon ports of Fabric mods), run on every pull request.

Fails when a mod folder holds binaries, oversized files or Mojang-looking files, or has no README or no
content/native half.
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
MODS = "fabric-mods"
BINARY_EXT = {".jar", ".class", ".dll", ".so", ".dylib", ".exe", ".hmod", ".hnmod", ".hpack", ".zip"}
MAX_BYTES = 2 * 1024 * 1024
MOD_ID = re.compile(r"^[a-z0-9_]+$")
# Names that only show up when someone copies the game itself in.
MOJANG_HINTS = ("net/minecraft/", "com/mojang/", "client.jar", "server.jar")


def tracked_files() -> list[pathlib.Path]:
    out = subprocess.run(["git", "ls-files", "-z", "--", MODS], cwd=ROOT, capture_output=True, check=True).stdout
    return [ROOT / p for p in out.decode().split("\0") if p]


def check(files: list[pathlib.Path]) -> list[str]:
    problems = []
    for f in files:
        rel = f.relative_to(ROOT).as_posix()
        if f.suffix.lower() in BINARY_EXT:
            problems.append(f"{rel}: binaries and built mods belong in Releases, not the repository")
        if f.is_file() and f.stat().st_size > MAX_BYTES:
            problems.append(f"{rel}: larger than {MAX_BYTES // 1024 // 1024} MiB")
        if any(h in rel for h in MOJANG_HINTS):
            problems.append(f"{rel}: looks like a Mojang file")
    mods = ROOT / MODS
    for d in sorted(p for p in mods.iterdir() if p.is_dir()) if mods.is_dir() else []:
        if not MOD_ID.match(d.name):
            problems.append(f"{MODS}/{d.name}: mod ids are lowercase letters, digits and _")
        if not (d / "README.md").is_file():
            problems.append(f"{MODS}/{d.name}: needs a README.md")
        content = (d / "content" / "horizon-mod.toml").is_file()
        native = (d / "native" / "horizon-mixin.toml").is_file()
        if not (content or native):
            problems.append(f"{MODS}/{d.name}: needs content/horizon-mod.toml or native/horizon-mixin.toml")
    return problems


def main() -> int:
    problems = check(tracked_files())
    for p in problems:
        print(f"error: {p}")
    print("ok" if not problems else f"{len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
