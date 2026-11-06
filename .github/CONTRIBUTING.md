# Contributing to Horizon Fabric

Thanks for helping. This page is short; please read all of it. Horizon's own guide still applies to the engine
code: [CONTRIBUTING.md](../CONTRIBUTING.md) and [CLAUDE.md](../CLAUDE.md).

## What goes where

- **The Fabric port in the engine** (`crates/horizon-protocol/src/fabric/`, the client glue, the docs): fixes and
  new Fabric API modules. A change that isn't about Fabric belongs in
  [horizon-rust/horizon](https://github.com/horizon-rust/horizon): it reaches this repository through the sync.
- **Mod ports** in [`fabric-mods/`](../fabric-mods): one folder per mod, see its README.

## How changes get in

- `main` is protected. Nobody pushes to it directly: every change is a **pull request**.
- A pull request is merged only after **an approving review** from the owner, with **all checks green** and **all
  review conversations resolved**. A new push after an approval needs a new approval.
- Feature pull requests are squash-merged. Upstream sync pull requests are merged with a merge commit.

## Steps

1. **Open an issue first** for a new mod port or a bigger change. Small fixes can go straight to a pull request.
2. **Fork** this repository and branch (`<mod-id>/<what>` or `fabric/<what>`).
3. Make the change, then run the checks:
   ```sh
   python3 tools/check_fabric_mods.py && python3 tools/check_public_tree.py
   cargo fmt --all -- --check
   cargo clippy -p horizon-protocol -p horizon-items --all-targets -- -D warnings
   cargo test -p horizon-protocol fabric::
   cd fabric-mods/<mod-id>/content && cargo horizon-mod check      # content mods
   ```
4. Open the pull request and fill in the template. One mod or one topic per pull request, ideally under ~500 lines.

## Rules

- **No Mojang files**: no game jars, vanilla textures, sounds or models, no decompiled source. **No Fabric mod jars**
  either, and no copied assets unless the original's licence allows it (name the source).
- **No binaries**: no `.jar`, `.class`, `.dll`, `.so`, `.dylib`, `.exe`, no built `.hmod`/`.hnmod`. Built files go to
  Releases, made from the reviewed source.
- **Native mods are reviewed line by line.** No network access, no file access outside the game folder, no
  processes, nothing obfuscated. Explain each hook in the pull request.
- **Tests with signal**: expected values come from the Fabric or vanilla source (or a Java run), never from the code
  under test. A server must never be able to crash the client.
- **Translations**: no hardcoded player-facing text; translation keys with at least `en_us`.
- **Licence**: your contribution is released under this repository's licence (MIT, as Horizon), or the licence in
  the mod's folder.
- **AI-written contributions** are fine when marked: the first line of the pull request says which tool wrote it.

## Security

Report problems privately: [SECURITY.md](SECURITY.md).
