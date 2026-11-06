# Developing Horizon Fabric

How to keep working on this repository (`constantinktz/horizon-fabric`): Horizon plus a port of Fabric API and
ports of Fabric mods. For people and agents alike. Horizon's own rules ([AGENTS.md](../AGENTS.md),
[CONTRIBUTING.md](../CONTRIBUTING.md)) still hold for everything; this page adds what is special here.

## The three parts and where they live

| Part | Where | Spec |
|---|---|---|
| Fabric API's network side (handshake, registry sync, splitter, menus, particles, recipes, attachments) | `crates/horizon-protocol/src/fabric/` (one file per Fabric module), client glue in `crates/horizon-client/src/net.rs` (`fn configure`, `fn receive_play`) and `game/remote_fabric*.rs` | Fabric API's source at the tag in `docs/fabric-servers.md` |
| Fabric API's mod side (events, registries) | Horizon's native mod hooks: `game/native_hooks.rs`, `tools/native-hook-fragments/*.json` | the module's `api/` package; the map is [fabric-api.md](fabric-api.md) |
| Ports of Fabric mods | `fabric-mods/<mod-id>/` ([its README](../fabric-mods/README.md)) | the original mod's source and in-game behaviour |

Server-side mod behaviour that every client needs (a mod's attribute, its play channel) goes into the engine
(`game/remote_fabric.rs`), not into `fabric-mods/`: a player can't be asked to install it.

## Upstream comes first

- `main` follows `horizon-rust/horizon` through the upstream sync (`.github/workflows/upstream-sync.yml`, every 6
  hours, a pull request merged with a **merge commit**). Never squash or rebase a sync.
- Keep the diff against upstream small and in files upstream doesn't touch: new files beat edits, an edit to a
  shared file (`net.rs`, `remote.rs`, `game.rs`) is a few lines that call into a Fabric file. Never reformat or move
  upstream code.
- Repository files of our own live in `.github/` (README, CONTRIBUTING, SECURITY, workflows) so the root files stay
  upstream's. Upstream's workflows are kept off here; ours are `fabric-checks.yml` and `upstream-sync.yml`.
- A change that isn't about Fabric belongs upstream: open it there, it comes back through the sync.
- A sync with conflicts opens an issue. Resolve locally: `git fetch upstream && git merge upstream/main`, keep
  upstream's side unless a Fabric line is at stake, run the checks below, open a pull request.

## Porting a Fabric API module

1. Find the module in Fabric API's source; read `impl/` for the wire format and the payload codecs (`StreamCodec`,
   `PacketCodec`), the `client/` part for what a client answers, and the mixins for where it hooks in.
2. Write the codec in `crates/horizon-protocol/src/fabric/<module>.rs`: constants named as Fabric names them,
   Fabric's size caps, a doc comment naming the Java class each item ports.
3. Decide what Horizon claims (channels it registers, serializers or attachments it accepts). Claim only what it
   really handles: a Fabric server sends vanilla data to a client that claims nothing.
4. Wire it with a few lines in `net.rs` (configuration) or `remote.rs` (play), state in a `game/remote_fabric_*.rs`.
5. Update `docs/fabric-servers.md` and the module's row in `docs/fabric-api.md`.

## Porting a Fabric mod

1. Licence first: it must allow a port; name it in the mod's README.
2. Content (blocks, items, recipes) → `content/` as a content mod; behaviour → `native/` with hooks from
   [fabric-api.md](fabric-api.md). A hook that is missing is added to the engine first, in its own pull request.
3. Port behaviour, not Java. No copied assets unless the licence allows it.
4. `python3 tools/check_fabric_mods.py`, `cargo horizon-mod check`, then play it and write down what you checked.

## Tests with signal

- Expected bytes come from the Fabric or vanilla Java source, hand-built from the codec, or from a Java run against
  the real jars. Never from the Rust encoder under test; a round trip alone proves nothing.
- Every decoder gets the malformed cases: every truncated prefix, Fabric's caps and cap + 1, negative and huge
  VarInts, unknown ids, random buffers. **A server must never be able to panic the client.**
- Where Fabric disconnects on bad data, Horizon may drop the payload and go on; say so in a comment.
- End to end against real Fabric servers: `python3 tools/fabric_e2e.py --profile minimal --profile
  vanilla-perfected --bin-dir target/fast` (offline servers on 127.0.0.1, ports 26170-26199). A module a server's
  mods use but Horizon doesn't answer shows as `unsupported`.

## Checks before a pull request

```sh
cargo fmt --all -- --check
cargo clippy -p horizon-protocol -p horizon-items -p horizon-client --lib --bins --tests -- -D warnings
cargo test -p horizon-protocol -p horizon-items --lib
cargo test -p horizon-client --lib fabric                          # the gameplay glue
python3 -m unittest tests.test_fabric_e2e
python3 tools/check_public_tree.py && python3 tools/check_fabric_mods.py
python3 tools/fabric_e2e.py --profile minimal --bin-dir target/fast   # when net.rs or remote.rs changed
```

CI (`fabric-checks.yml`) runs the first, the protocol tests and the two tree checks.

## Agents

- Mark what you post: a pull request body starts with `> 🤖 Opened by <login>'s AI agent (<tool, model>), not by
  <login> personally.`; commits end with a `Co-Authored-By:` line for the agent.
- Work in a branch or worktree of your own, never on `main`. Several agents at once: one file each, shared files
  only by the one integrating.
- Runs get scratch `HORIZON_HOME`/`XDG_*` dirs (Horizon's AGENTS.md); servers you start are offline, on 127.0.0.1,
  and stopped when you finish. Never connect to someone's real server for a test.
- Never download or run third-party mod jars for a test; use Fabric API itself or a server folder the owner
  prepared.

## What's open

The honest list lives in [fabric-servers.md](fabric-servers.md) and [fabric-api.md](fabric-api.md). The biggest:
servers whose mods add blocks (every block state id from the network must be clamped first: chunks, `set_block`,
entity data, particles, projectiles), mod data component types, Fabric's built-in custom ingredients, and the
native-mod side of data attachments.
