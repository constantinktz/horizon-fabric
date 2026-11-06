---
name: fabric-port
description: Port a part of Fabric API or a Fabric mod to Horizon in the horizon-fabric repository, against Fabric's own source and with tests that can't come from the code under test. Use for "make Horizon support Fabric module X", "port Fabric mod Y", a Fabric server that refuses or desyncs Horizon, or an upstream sync conflict in a Fabric file.
---

# Porting Fabric to Horizon

Full guide: `docs/fabric-development.md`. Module map: `docs/fabric-api.md`. Server join: `docs/fabric-servers.md`.

## 1. Find out what crosses the wire
- Get Fabric API's source at the tag `docs/fabric-servers.md` names (`git clone --depth 1 --branch <tag>
  https://github.com/FabricMC/fabric-api`), outside the repository.
- In the module: `impl/**/*Payload*.java` and `*Codec*` for bytes, the `client/` package for what a client answers,
  `mixin/` for where it hooks into vanilla. Constants (channel ids, caps, markers) are copied by name.
- No wire format (events, registries for Java mods) → it maps onto a native hook: `docs/fabric-api.md`.

## 2. Write it where it belongs
- Codec: `crates/horizon-protocol/src/fabric/<module>.rs`, doc comments naming the Java class each item ports.
- Glue: a few lines in `crates/horizon-client/src/net.rs` (configuration: `fn configure`; play: `fn receive_play`)
  or `game/remote.rs`, state in `game/remote_fabric_*.rs`. Keep edits to upstream files minimal: the upstream sync
  merges into them every 6 hours.
- A mod port: `fabric-mods/<mod-id>/` with `content/` and/or `native/` and a README naming the original and its
  licence.
- Claim only what Horizon handles (channels, serializers, attachment ids).

## 3. Test with signal
- Expected bytes hand-built from the Java codec or from a Java run against the real jars, never from the encoder.
- Truncated prefixes, caps and cap + 1, bad VarInts, unknown ids, random buffers: decoding returns `Err`, never
  panics. A server must never crash the client.
- `cargo test -p horizon-protocol fabric::`, `cargo test -p horizon-client --lib fabric`, clippy `-D warnings`,
  `cargo fmt --all -- --check`, `python3 tools/check_fabric_mods.py`.
- End to end: `python3 tools/fabric_e2e.py --profile minimal --bin-dir target/fast` (scratch dirs, offline server
  on 127.0.0.1). Third-party mod downloads need the owner's OK.

## 4. Document
- The module's row in `docs/fabric-api.md`, the behaviour in `docs/fabric-servers.md`, what is still missing,
  honestly.
- Pull request body starts with the agent line from `docs/fabric-development.md`; never push to `main`.
