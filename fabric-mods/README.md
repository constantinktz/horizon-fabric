# Fabric mods for Horizon

Ports of Fabric mods to Horizon. A Fabric jar can't run in Horizon (its code patches Minecraft's Java classes, and
Horizon has none), so each mod here is rebuilt as one of Horizon's own mod kinds:

| Kind | File | What it is |
|---|---|---|
| **Content mod** | `.hmod` | Data and assets only: blocks, items, recipes, loot, tags, models, textures. No code. |
| **Native mod** | `.hnmod` | A [horizon-mixins](https://github.com/horizon-rust/horizon-mixins) bundle: Rust that hooks into the game's own source before the launcher compiles it. |

How both work: [docs/mods.md](../docs/mods.md), [docs/native-mods.md](../docs/native-mods.md). Which Fabric API
event maps to which native hook: [docs/fabric-api.md](../docs/fabric-api.md).

A port that a Fabric **server** needs on every client (it adds play channels, synced attachments, attributes)
belongs in Horizon itself instead, like Vehicle Upgrade's `airborne_mining_speed`
(`crates/horizon-client/src/game/remote_fabric.rs`).

## Layout

```
fabric-mods/<mod-id>/
  README.md          the original mod (name, link, licence), what the port covers, what it doesn't
  content/           a content mod: horizon-mod.toml, assets/, data/          (optional)
  native/            a native mod: horizon-mixin.toml and its .rs files      (optional)
  LICENSE            when it differs from this repository's
```

A mod has a `content/` half, a `native/` half, or both. Mod ids are lowercase letters, digits and `_`; use the
original mod's id where it has one.

## Installing

1. Download the file from [Releases](https://github.com/constantinktz/horizon-fabric/releases) (tags are
   `<mod-id>-v<version>`).
2. Put a `.hmod` in `horizon-mods/` and a `.hnmod` in `horizon-native-mods/` of your game folder.
3. Turn it on in Options > Mods and restart the game.

**Native mods are native code that runs with your rights.** They are not sandboxed. Every one here is reviewed
before it is merged, but read the source of anything you build and run.

## Building

```sh
cd fabric-mods/<mod-id>/content && cargo horizon-mod build && cargo horizon-mod check
```

The launcher builds a native mod for your instance when you install its `.hnmod`.

## Porting a Fabric mod

1. Check the original's licence: it must allow a port. Say which licence in the mod's README.
2. Read its source and write down what it does in game. Port behaviour, not Java: assets you may reuse only when the
   licence allows it.
3. Find the hooks you need in [docs/fabric-api.md](../docs/fabric-api.md). A missing hook is an issue first.
4. Test it in game and describe what you checked in the pull request.

Contributing rules: [.github/CONTRIBUTING.md](../.github/CONTRIBUTING.md).
