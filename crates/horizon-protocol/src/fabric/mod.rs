//! Joining Fabric servers: the configuration-phase handshake of Fabric API's
//! networking (`minecraft:register`, `c:version`, `c:register`) and its
//! registry sync (`fabric:registry/sync`), answered as a Fabric client does
//! (Fabric API 0.162: `ServerConfigurationNetworkAddon`, `CommonPacketsImpl`,
//! `RegistrySyncManager`, `RegistrySyncPayload`).
//!
//! A Fabric server whose mods add entries to synced registries (an attribute,
//! a menu type ...) refuses a client that doesn't take part in registry sync:
//! "This server requires Fabric Loader and Fabric API installed on your
//! client!". Its configuration starts with `minecraft:register` (the server's
//! channels) and a ping; a client that registers channels of its own before
//! it answers the ping is a modded one. Horizon registers [`CLIENT_CHANNELS`],
//! agrees on common packet version 1, announces no play channels (so mods send
//! it nothing of their own) and takes the server's id tables: the vanilla
//! entries keep the ids Horizon knows, the mods' entries come after them.
use crate::{
    Error, Result,
    buf::{Reader, Writer},
    registry::{Builtin, Ids},
};
use std::collections::HashMap;

pub mod attachment;
pub mod ingredient;
pub mod menu;
pub mod particles;
pub mod recipe;
pub mod splitter;
pub mod state_ids;

/// The channel registration payload (`RegistrationPayload.REGISTER`).
pub const REGISTER: &str = "minecraft:register";
/// `CommonVersionPayload`: the common packet versions each side supports.
pub const VERSION: &str = "c:version";
/// `CommonRegisterPayload`: the channels each side receives in a phase.
pub const COMMON_REGISTER: &str = "c:register";
/// `RegistrySyncPayload`: the server's id tables of its modded registries.
pub const REGISTRY_SYNC: &str = "fabric:registry/sync";
/// `SyncCompletePayload`: the client applied the tables.
pub const REGISTRY_SYNC_COMPLETE: &str = "fabric:registry/sync/complete";
/// `CommonPacketsImpl.SUPPORTED_COMMON_PACKET_VERSIONS`.
pub const COMMON_VERSION: i32 = 1;
/// `CommonRegisterPayload.PLAY_PROTOCOL`.
pub const PLAY: &str = "play";
/// What Horizon registers: the server's `canSend` checks exactly these
/// before it negotiates the version, the play channels and the tables.
pub const CLIENT_CHANNELS: [&str; 3] = [VERSION, COMMON_REGISTER, REGISTRY_SYNC];
/// The registries whose mod entries Horizon can't take anywhere: their ids
/// sit in chunk palettes (block states follow block raw ids) and item stacks.
/// Blocks, items and fluids are taken when the mods' entries all come after
/// vanilla's ([`state_ids::appended`]); a mod's data component type never is,
/// since its payload in an item stack has no length to skip it by.
pub const STRICT: [&str; 4] = [
    "minecraft:block",
    "minecraft:item",
    "minecraft:fluid",
    "minecraft:data_component_type",
];

/// `AbstractChanneledNetworkAddon.MAX_CHANNELS`.
const MAX_CHANNELS: usize = 8192;
/// `GlobalReceiverRegistry.DEFAULT_CHANNEL_NAME_MAX_LENGTH`.
const MAX_CHANNEL_NAME: usize = 128;
/// `FriendlyByteBuf.readUtf`'s default limit.
const MAX_STRING: usize = 32767;

/// A server that sends this in its first `minecraft:register` runs Fabric
/// API: it waits for the client's channels before it configures it.
pub fn is_fabric_server(server_channels: &[String]) -> bool {
    server_channels
        .iter()
        .any(|c| c == VERSION || c == REGISTRY_SYNC_COMPLETE)
}

/// `RegistrationPayload.write`: the channel names, NUL between them.
pub fn encode_channels(channels: &[&str]) -> Vec<u8> {
    channels.join("\0").into_bytes()
}

/// `RegistrationPayload.read`: the names between NULs; empty and malformed
/// ones are skipped as Fabric skips them.
pub fn decode_channels(data: &[u8]) -> Result<Vec<String>> {
    if data.len() > MAX_CHANNELS * (MAX_CHANNEL_NAME + 1) {
        return Err(Error::new("channel registration payload is too large"));
    }
    let mut out = Vec::new();
    for name in data.split(|&b| b == 0) {
        if name.len() > MAX_CHANNEL_NAME {
            return Err(Error::new("channel identifier is too long"));
        }
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        if name.is_empty() || !identifier_ok(name) {
            continue;
        }
        if out.len() >= MAX_CHANNELS {
            return Err(Error::new("too many channel identifiers"));
        }
        out.push(name.to_owned());
    }
    Ok(out)
}

/// `CommonVersionPayload`: a VarInt array.
pub fn encode_version(versions: &[i32]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(versions.len() as i32);
    for &v in versions {
        w.varint(v);
    }
    w.data
}

pub fn decode_version(data: &[u8]) -> Result<Vec<i32>> {
    let mut r = Reader::new(data);
    let n = r.count(r.remaining())?;
    (0..n).map(|_| r.varint()).collect()
}

/// `CommonPacketsImpl.getHighestCommonVersion` of the server's versions and
/// Horizon's.
pub fn negotiate(server: &[i32]) -> Option<i32> {
    server.contains(&COMMON_VERSION).then_some(COMMON_VERSION)
}

/// `CommonRegisterPayload.write`: the version, the phase, then the channels.
pub fn encode_common_register(version: i32, protocol: &str, channels: &[&str]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(version);
    w.string(protocol);
    w.varint(channels.len() as i32);
    for c in channels {
        w.string(c);
    }
    w.data
}

/// One registry of a `RegistrySyncPayload`: each entry's name and the id the
/// server gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncedRegistry {
    pub id: String,
    /// `RegistryAttribute.OPTIONAL`: a client without the registry may join.
    pub optional: bool,
    pub entries: Vec<(String, i32)>,
}

/// `RegistrySyncPayload.read`: registries grouped by namespace, each entry's
/// namespace once, consecutive ids in bulks that start relative to the end of
/// the bulk before (across namespaces); an empty namespace is `minecraft`.
pub fn decode_registry_sync(data: &[u8]) -> Result<Vec<SyncedRegistry>> {
    let mut r = Reader::new(data);
    let mut out = Vec::new();
    for _ in 0..r.count(r.remaining())? {
        let registry_namespace = namespace(r.string(MAX_STRING)?);
        for _ in 0..r.count(r.remaining())? {
            let path = r.string(MAX_STRING)?;
            let optional = r.u8()? & 1 != 0;
            let mut entries = Vec::new();
            let mut last: i32 = 0;
            for _ in 0..r.count(r.remaining())? {
                let entry_namespace = namespace(r.string(MAX_STRING)?);
                for _ in 0..r.count(r.remaining())? {
                    let start = r.varint()?;
                    let size = r.count(r.remaining())?;
                    let mut id = last
                        .checked_add(start)
                        .and_then(|v| v.checked_sub(1))
                        .ok_or_else(|| Error::new("registry sync: id out of range"))?;
                    for _ in 0..size {
                        id = id
                            .checked_add(1)
                            .ok_or_else(|| Error::new("registry sync: id out of range"))?;
                        let entry_path = r.string(MAX_STRING)?;
                        entries.push((format!("{entry_namespace}:{entry_path}"), id));
                    }
                    last = id;
                }
            }
            out.push(SyncedRegistry {
                id: format!("{registry_namespace}:{path}"),
                optional,
                entries,
            });
        }
    }
    if r.remaining() > 0 {
        return Err(Error::new("registry sync: trailing bytes"));
    }
    Ok(out)
}

/// The tables Horizon plays with after a registry sync, by registry: what a
/// Fabric client's `RegistrySyncManager` remaps its own registries to. A
/// vanilla entry numbered differently, or a mod entry in a [`STRICT`]
/// registry, can't be played: the error lists the namespaces involved.
pub fn remap(
    builtin: &Builtin,
    synced: &[SyncedRegistry],
) -> std::result::Result<HashMap<String, Ids>, Vec<String>> {
    let mut tables = HashMap::new();
    let mut unsupported = Vec::new();
    for registry in synced {
        let known = builtin.get(&registry.id);
        let mut names: Vec<String> = Vec::new();
        for (name, id) in &registry.entries {
            let Ok(index) = usize::try_from(*id) else {
                unsupported.push(namespace_of(name).to_owned());
                continue;
            };
            match known.and_then(|k| k.id(name)) {
                // The server moved a vanilla entry.
                Some(ours) if ours != *id => unsupported.push(namespace_of(name).to_owned()),
                Some(_) => {}
                // A mod's component type: an item stack holding it can't be read.
                None if known.is_some() && registry.id == "minecraft:data_component_type" => {
                    unsupported.push(namespace_of(name).to_owned())
                }
                None => {}
            }
            if index >= names.len() {
                names.resize(index + 1, String::new());
            }
            names[index] = name.clone();
        }
        // Mod blocks, items and fluids after every vanilla one keep vanilla's
        // ids and block state ids; the mods' states are placeholders.
        if let Some(known) = known
            && STRICT.contains(&registry.id.as_str())
            && let Err(namespaces) = state_ids::appended(known, &registry.entries)
        {
            unsupported.extend(namespaces);
        }
        tables.insert(registry.id.clone(), Ids::from_names(names));
    }
    if unsupported.is_empty() {
        Ok(tables)
    } else {
        unsupported.sort();
        unsupported.dedup();
        Err(unsupported)
    }
}

/// `RegistrySyncPayload.unoptimizeNamespace`.
fn namespace(s: String) -> String {
    if s.is_empty() {
        "minecraft".to_owned()
    } else {
        s
    }
}

fn namespace_of(name: &str) -> &str {
    name.split_once(':').map_or("minecraft", |(ns, _)| ns)
}

/// `Identifier.tryParse`: a namespace and a path of the allowed characters.
fn identifier_ok(s: &str) -> bool {
    let (ns, path) = s.split_once(':').unwrap_or(("minecraft", s));
    !path.is_empty()
        && ns
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b))
        && path
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-./".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload written by hand the way `RegistrySyncPayload.write` lays it
    /// out: two registry namespaces; in `minecraft:attribute` the vanilla
    /// entries 0, 1 and 3 (two bulks, the gap at 2) and a mod's entry 4,
    /// whose bulk starts from the end of the vanilla one.
    fn payload() -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(2); // registry namespaces
        w.string(""); // minecraft
        w.varint(1);
        w.string("attribute");
        w.u8(0);
        w.varint(2); // entry namespaces
        w.string("");
        w.varint(2); // bulks
        w.varint(0); // starts at 0
        w.varint(2);
        w.string("armor");
        w.string("armor_toughness");
        w.varint(2); // 1 + 2 = 3
        w.varint(1);
        w.string("block_break_speed");
        w.string("vehicleupgrade");
        w.varint(1);
        w.varint(1); // 3 + 1 = 4
        w.varint(1);
        w.string("airborne_mining_speed");
        w.string("iteminteractions");
        w.varint(1);
        w.string("item_storage_type");
        w.u8(1); // optional
        w.varint(1);
        w.string("iteminteractions");
        w.varint(1);
        w.varint(0);
        w.varint(2);
        w.string("empty");
        w.string("container");
        w.data
    }

    #[test]
    fn registry_sync_decodes_bulks_and_namespaces() {
        let got = decode_registry_sync(&payload()).unwrap();
        assert_eq!(
            got,
            vec![
                SyncedRegistry {
                    id: "minecraft:attribute".into(),
                    optional: false,
                    entries: vec![
                        ("minecraft:armor".into(), 0),
                        ("minecraft:armor_toughness".into(), 1),
                        ("minecraft:block_break_speed".into(), 3),
                        ("vehicleupgrade:airborne_mining_speed".into(), 4),
                    ],
                },
                SyncedRegistry {
                    id: "iteminteractions:item_storage_type".into(),
                    optional: true,
                    entries: vec![
                        ("iteminteractions:empty".into(), 0),
                        ("iteminteractions:container".into(), 1),
                    ],
                },
            ]
        );
    }

    #[test]
    fn registry_sync_rejects_cut_and_padded_payloads() {
        let full = payload();
        assert!(decode_registry_sync(&full[..full.len() - 3]).is_err());
        let mut padded = full;
        padded.push(0);
        assert!(decode_registry_sync(&padded).is_err());
    }

    fn builtin() -> Builtin {
        Builtin::parse(
            r#"{
                "minecraft:attribute": {"entries": {
                    "minecraft:armor": {"protocol_id": 0},
                    "minecraft:armor_toughness": {"protocol_id": 1},
                    "minecraft:attack_damage": {"protocol_id": 2},
                    "minecraft:block_break_speed": {"protocol_id": 3}
                }},
                "minecraft:block": {"entries": {
                    "minecraft:air": {"protocol_id": 0},
                    "minecraft:stone": {"protocol_id": 1}
                }},
                "minecraft:item": {"entries": {
                    "minecraft:air": {"protocol_id": 0}
                }},
                "minecraft:data_component_type": {"entries": {
                    "minecraft:custom_data": {"protocol_id": 0}
                }}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn mod_entries_after_the_vanilla_ones_are_taken() {
        let synced = decode_registry_sync(&payload()).unwrap();
        let tables = remap(&builtin(), &synced).unwrap();
        let attributes = &tables["minecraft:attribute"];
        assert_eq!(
            attributes.name(4),
            Some("vehicleupgrade:airborne_mining_speed")
        );
        assert_eq!(attributes.id("minecraft:block_break_speed"), Some(3));
        let storage = &tables["iteminteractions:item_storage_type"];
        assert_eq!(storage.id("iteminteractions:container"), Some(1));
    }

    #[test]
    fn moved_vanilla_entries_and_unplaceable_mod_entries_are_refused() {
        let moved = SyncedRegistry {
            id: "minecraft:attribute".into(),
            optional: false,
            entries: vec![
                ("minecraft:armor".into(), 1),
                ("minecraft:armor_toughness".into(), 0),
            ],
        };
        assert_eq!(
            remap(&builtin(), &[moved]).err(),
            Some(vec!["minecraft".into()])
        );
        // A mod block where vanilla's stone was: every state after it moves.
        let blocks = SyncedRegistry {
            id: "minecraft:block".into(),
            optional: false,
            entries: vec![
                ("minecraft:air".into(), 0),
                ("copper_age:vent".into(), 1),
                ("minecraft:stone".into(), 2),
            ],
        };
        assert_eq!(
            remap(&builtin(), &[blocks]).err(),
            Some(vec!["copper_age".into(), "minecraft".into()])
        );
        // A server without stone.
        let missing = SyncedRegistry {
            id: "minecraft:block".into(),
            optional: false,
            entries: vec![("minecraft:air".into(), 0), ("copper_age:vent".into(), 2)],
        };
        assert_eq!(
            remap(&builtin(), &[missing]).err(),
            Some(vec!["minecraft".into()])
        );
        // A mod's component type, even after vanilla's.
        let components = SyncedRegistry {
            id: "minecraft:data_component_type".into(),
            optional: false,
            entries: vec![
                ("minecraft:custom_data".into(), 0),
                ("copper_age:charge".into(), 1),
            ],
        };
        assert_eq!(
            remap(&builtin(), &[components]).err(),
            Some(vec!["copper_age".into()])
        );
    }

    #[test]
    fn mod_blocks_and_items_after_vanilla_are_taken() {
        let blocks = SyncedRegistry {
            id: "minecraft:block".into(),
            optional: false,
            entries: vec![
                ("minecraft:air".into(), 0),
                ("minecraft:stone".into(), 1),
                ("copper_age:vent".into(), 2),
                ("copper_age:pipe".into(), 3),
            ],
        };
        let items = SyncedRegistry {
            id: "minecraft:item".into(),
            optional: false,
            entries: vec![("minecraft:air".into(), 0), ("copper_age:wrench".into(), 1)],
        };
        let tables = remap(&builtin(), &[blocks, items]).unwrap();
        assert_eq!(tables["minecraft:block"].id("minecraft:stone"), Some(1));
        assert_eq!(tables["minecraft:block"].name(3), Some("copper_age:pipe"));
        assert_eq!(tables["minecraft:item"].name(1), Some("copper_age:wrench"));
    }

    #[test]
    fn channels_split_on_nul_and_skip_bad_names() {
        let data = b"c:version\0\0fabric:registry/sync/complete\0Bad Name\0c:register";
        assert_eq!(
            decode_channels(data).unwrap(),
            ["c:version", "fabric:registry/sync/complete", "c:register"]
        );
        assert_eq!(
            encode_channels(&CLIENT_CHANNELS),
            b"c:version\0c:register\0fabric:registry/sync"
        );
    }

    #[test]
    fn a_fabric_server_is_recognised_by_its_channels() {
        let fabric = decode_channels(b"c:version\0fabric:registry/sync/complete").unwrap();
        assert!(is_fabric_server(&fabric));
        let other = decode_channels(b"bungeecord:main").unwrap();
        assert!(!is_fabric_server(&other));
    }

    #[test]
    fn version_one_is_agreed() {
        // writeVarIntArray: the length, then each value.
        assert_eq!(encode_version(&[1]), [1, 1]);
        assert_eq!(decode_version(&[2, 1, 2]).unwrap(), [1, 2]);
        assert_eq!(negotiate(&[1, 2]), Some(1));
        assert_eq!(negotiate(&[2]), None);
    }

    #[test]
    fn common_register_lists_the_play_channels() {
        let mut expected = vec![1, 4];
        expected.extend_from_slice(b"play");
        expected.extend_from_slice(&[1, 3]);
        expected.extend_from_slice(b"a:b");
        assert_eq!(encode_common_register(1, PLAY, &["a:b"]), expected);
    }
}
