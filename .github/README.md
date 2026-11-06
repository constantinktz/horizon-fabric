# Horizon Fabric

[Horizon](https://github.com/horizon-rust/horizon) (Minecraft: Java Edition 26.3 rewritten in Rust) that speaks
**Fabric API**: it joins Fabric servers the way a Fabric client does, and its native mods get hooks modelled on
Fabric API's events.

This repository follows `horizon-rust/horizon`: an Action merges upstream's `main` every 6 hours and opens a pull
request ([upstream-sync](workflows/upstream-sync.yml)). Everything else is Horizon's own code and docs; the
upstream README is [here](../README.md).

## What it adds

| Fabric API module | What Horizon does | Code |
|---|---|---|
| networking-api-v1 | `minecraft:register`, `c:version`, `c:register` in configuration and play; Fabric's packet splitter for oversized payloads | `crates/horizon-protocol/src/fabric/{mod,splitter}.rs`, `splitter/channels.rs` |
| registry-sync-v0 | takes the server's id tables (mod attributes, menus ...); block state id rebuild (`StateIdTracker`); unknown block state ids no longer panic | `fabric/mod.rs`, `fabric/state_ids.rs`, `horizon-items/src/blocks.rs` |
| menu-api-v1 | `open_screen` for extended menus; a mod menu shaped like a chest or hopper opens as one, others close cleanly | `fabric/menu.rs`, `game/remote_fabric_menu.rs` |
| particles-v1 | the extended block particle option (block position for the tint) | `fabric/particles.rs`, `entity_data.rs` |
| recipe-api-v1 | custom ingredient and recipe sync answered (Horizon claims none, so the server sends vanilla data) | `fabric/{ingredient,recipe}.rs` |
| data-attachment-api-v1 | attachment sync codecs and a client store, for native mods that accept attachment types | `fabric/attachment.rs`, `game/remote_fabric_attach.rs` |
| the other 39 modules | mapped onto Horizon's native mod hooks, with new client lifecycle and chunk hooks | [docs/fabric-api.md](../docs/fabric-api.md) |

**Mods:** Horizon ports of Fabric mods live in [`fabric-mods/`](../fabric-mods), one folder per mod, with their files
on [Releases](https://github.com/constantinktz/horizon-fabric/releases).

Still refused: servers whose mods add blocks or data component types (their state counts and payloads aren't on
the wire). The details, and what each module still lacks: [docs/fabric-servers.md](../docs/fabric-servers.md),
[docs/fabric-api.md](../docs/fabric-api.md).

## Build and test

```sh
cargo build --profile fast -p horizon-client
cargo test -p horizon-protocol fabric::
python3 tools/fabric_e2e.py --profile minimal --bin-dir target/fast   # a real Fabric server, end to end
```

The game needs the vanilla data from your own Minecraft install: see
[docs/parity-workflow.md](../docs/parity-workflow.md).

## Contributing

`main` is protected: every change is a pull request, merged after the owner's approval with green checks. Upstream
sync pull requests are merged with a **merge commit** (not squash), so the next sync starts from them. Read
[CONTRIBUTING.md](CONTRIBUTING.md); security problems go to [SECURITY.md](SECURITY.md), not a public issue.

Fabric API is © FabricMC, Apache-2.0; this port reimplements its network formats from its source. Horizon is MIT.
Not affiliated with Mojang Studios, Microsoft or FabricMC.
