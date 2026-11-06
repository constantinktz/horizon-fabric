# Joining Fabric servers

A Fabric server whose mods add entries to synced registries (an attribute, a menu type, a
registry of their own) refuses vanilla clients: "This server requires Fabric Loader and Fabric
API installed on your client!". Horizon answers Fabric API's configuration handshake as a Fabric
client does, so it joins such a server when the mods' entries are ones it can play without
their code. Code: `horizon_protocol::fabric` (the payloads and the remap), `net.rs`'s
`configure` (the handshake), `game/remote.rs` `set_registries` (the tables) and
`game/remote_fabric.rs` (what the server's mods change for every client).

## The handshake

From Fabric API 0.162's sources (`ServerConfigurationNetworkAddon`, `CommonPacketsImpl`,
`RegistrySyncManager`, `RegistrySyncPayload`):

1. At the start of configuration the server sends `minecraft:register` with its channels, then a
   ping (`0xFAB71C`). A client that registers channels of its own before its pong is a modded
   one; one that answers the ping first is taken for vanilla. When the server's list names
   `c:version` or `fabric:registry/sync/complete`, Horizon registers `c:version`, `c:register` and
   `fabric:registry/sync`, and only those: the server's `canSend` checks are what decide which
   configuration tasks it runs.
2. `c:version`: the server offers its common packet versions, Horizon answers 1.
3. `c:register` (phase `play`): the server lists its play channels, Horizon lists none. Mods
   that check for their client half (`isModPresentClientside`, Fabric's `canSend`) then treat
   Horizon as a vanilla client and send it nothing of their own.
4. `fabric:registry/sync`: the id tables of every synced registry a mod added to (registries
   grouped by namespace, each entry's namespace once, consecutive ids in bulks). Horizon checks
   them against its built-in tables and answers `fabric:registry/sync/complete`.

Horizon's brand stays `vanilla`. Login-phase queries (Controlify's `controlify:handshake`)
are answered "not understood", as before.

## The tables

Fabric appends mods' entries after the vanilla ones, so the vanilla entries keep their ids. The
server's tables replace the built-in ones for the rest of the connection (also across a
reconfiguration that doesn't sync again), as a Fabric client remaps its registries. Names
then resolve to the mods' ids: an attribute snapshot, a menu type, a sound event.

## Mod blocks, items, components and particles

Packets carry block states, items, data component types and particle types by raw id, inside
chunk palettes, item stacks and particle options. A server's mods may add to those registries as
long as all of their entries come after vanilla's (`fabric::state_ids::appended`; Fabric numbers
them so): vanilla's ids, and vanilla's block state ids, are then exactly Horizon's, whatever the
mods' state counts. Every id from the first mod entry on reads as unknown
(`fabric::state_ids::Bounds`, set on the connection's thread after the sync):

| What | Read as |
|---|---|
| A mod block's state in a chunk, a block update, a falling block, a break or brush effect | stone (`PLACEHOLDER_STATE`), so it stands and collides |
| A mod block's state in entity data (an enderman's block) or a block particle | nothing; the particle shows stone |
| A mod item in a stack | no item (the slot looks empty; the server keeps the real one) |
| A stack holding a mod's data component, or a mod's particle | the packet can't be read and is skipped (no length to skip the value by); the first time, chat says `horizon.multiplayer.fabric_unreadable` |

Mod block states are never another block's: Horizon's own content mods number theirs after
vanilla too, and a server's id there is still the placeholder. The join is still refused with
`horizon.multiplayer.fabric_unsupported` (the namespaces involved) when the server numbers a
vanilla entry differently, lacks one, or puts a mod entry among vanilla's in one of those
registries (`minecraft:block`, `minecraft:item`, `minecraft:fluid`,
`minecraft:data_component_type`, `minecraft:particle_type`).

Not done: drawing mod blocks as themselves (their state counts and models aren't on the wire),
reading a mod's component values, Fabric API's split payloads for a registry sync over 1 MiB (a
server with very many mod entries), and channels of the mods themselves. Not tested live: no
server with block mods is in the end-to-end tool, which runs no third-party mods.

## Mods ported for every client

What a Fabric server's mods apply to every player, whatever the client, is ported in
`game/remote_fabric.rs`:

- **Vehicle Upgrade** (`vehicleupgrade:airborne_mining_speed`, `AirborneMiningSpeedHandler`): an
  airborne player's destroy speed is multiplied by the attribute × 5. The attribute is 0.2, and
  riding adds a transient `vehicleupgrade:riding` modifier (×5), so a rider mines at ground speed.
  Horizon keeps the player's synced mod attributes by name and applies the factor after vanilla's
  ÷5, only on a server that syncs the attribute.

## Checked against

Vanilla Perfected 26.3 (Fabric Loader 0.19.5, Fabric API 0.162.0+26.3, its 31 server mods plus
C2ME, ScalableLux, Krypton, Chunky, spark, Alternate Current, LuckPerms, Vanilla Permissions,
AntiXray and MinerTrack), offline mode on localhost: the server syncs
`vehicleupgrade:airborne_mining_speed` (attribute 40), `vehicleupgrade:equipment_user` (menu 25)
and `iteminteractions:item_storage_type` (optional). The headless client
(`HORIZON_NET_REPORT`) joins, loads 117 chunks, chats and runs `/list`; driven with
`horizon-ctl`, it rides a saddled horse (the server reports the attribute at 1.0), mines stone
from it in 13 ticks against 16 on the ground, opens a chest, and walks with no decode error.
Without the handshake the same server refuses it with the message above.

## End to end

`tools/fabric_e2e.py` runs the headless client against real Fabric servers and checks both sides'
logs. It starts each server in a scratch folder (offline mode, `127.0.0.1`, a free port in
26170-26199, its console fed through `tail -f`), with a log4j config that turns Fabric API's
registry sync and networking loggers to DEBUG, so the server's own log says which registries it
synced and what the client sent in configuration. It stops every process it started and prints a
JSON summary.

```sh
python3 tools/fabric_e2e.py --profile minimal --profile vanilla-perfected --bin-dir target/fast
```

Profiles: `minimal` (Fabric Loader 0.19.5 and Fabric API 0.162.0+26.3, downloaded once into
`.local/fabric-e2e/`, a flat world) and `vanilla-perfected` (a copy of a prepared server folder,
`--server-dir`; the source is never touched). It never downloads or runs third-party mods.

Checks: joined, chunks received, the chat line and `/list` came back, no undecodable packet, no
disconnect, the client answered `fabric:registry/sync/complete`, and one per Fabric API module
(splitter, menu, recipe sync, ingredient sync, attachment sync, particles, state ids). A module
check finds out from the server's mod jars (the Fabric API classes their code references) and the
synced registries whether a mod exercises it, and says `skipped` when none does. A module a mod
uses but Horizon doesn't take part in yet is `unsupported`; `--strict` makes that a failure. A
server Horizon declines (`horizon.multiplayer.fabric_unsupported`) is `unsupported` too, not a
failure.
