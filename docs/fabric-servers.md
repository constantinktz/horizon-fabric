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

The join is refused with `horizon.multiplayer.fabric_unsupported` (the namespaces involved) when
the server numbers a vanilla entry differently, or when a mod adds to a registry whose ids sit in
bulk data Horizon decodes: `minecraft:block`, `minecraft:item`, `minecraft:fluid` and
`minecraft:data_component_type` (chunk palettes and item stacks, where an unknown id breaks what
follows). Such a server needs its content mods ported first ([modpacks.md](modpacks.md)).

Not done: Fabric API's split payloads for a registry sync over 1 MiB (a server with very many
mod entries), the attachment sync (`fabric:accepted_attachments_v1`, never asked for since
Horizon doesn't register it), and channels of the mods themselves.

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
