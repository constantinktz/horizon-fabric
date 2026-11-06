//! Block state ids after a Fabric registry sync (fabric-registry-sync-v0's
//! `StateIdTracker`, registered by `BootstrapMixin` for
//! `Block.BLOCK_STATE_REGISTRY` and `Fluid.FLUID_STATE_REGISTRY`).
//!
//! A state id is not synced: after a remap, `StateIdTracker.recalcStateMap`
//! clears the `IdMapper` and adds every block's `getPossibleStates()` in raw
//! id order (an `Int2ObjectRBTreeMap`, so gaps between raw ids take no
//! states). A Fabric client has the mods, so it knows each modded block's
//! state count; `RegistrySyncPayload` carries only names and raw ids, and
//! Horizon knows the counts of vanilla's blocks alone. Entries the server
//! didn't send are client-only (`MappedRegistryMixin.remap`, `REMOTE`): they
//! get raw ids after the server's last, so their states come after all of the
//! server's and don't move any of them.
//!
//! What Horizon can still place: every block whose count it knows, up to the
//! first block whose count it doesn't. A block it knows after one it doesn't
//! starts at an id no one can tell, so that layout is refused. The states from
//! the first unknown block on are a mod's: [`ServerStates::horizon`] answers
//! `None` for them, and the caller shows a placeholder.
//!
//! The fluid state table is rebuilt the same way, but fluid state ids never
//! cross the network (a fluid's state rides on its block's), so only the
//! fluid registry's own ids matter, and those are names.
use crate::registry::{Builtin, Ids};
use horizon_world::registry::{BlockRegistry, BlockStateId};
use std::cell::Cell;
use std::collections::HashMap;

/// One block's states in a state table: the first id and how many.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub first: u32,
    pub count: u32,
}

/// Horizon's own vanilla state table: each block's first state id and count.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    blocks: HashMap<String, Span>,
    /// One past the last vanilla state id.
    total: u32,
}

impl Layout {
    /// The `minecraft:` blocks of Horizon's registry, by runs of the same
    /// name. A content mod's blocks are left out: their state counts are
    /// Horizon's templates', not the mod's own.
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let mut layout = Self::default();
        let mut id = 0u32;
        while let Some(state) = registry.state(BlockStateId(id)) {
            let name = &state.name;
            let first = id;
            id += 1;
            while registry
                .state(BlockStateId(id))
                .is_some_and(|s| s.name == *name)
            {
                id += 1;
            }
            if name.starts_with("minecraft:") {
                layout.insert(name, first, id - first);
            }
        }
        layout
    }
    /// The data generator's block report (`reports/blocks.json`): each
    /// block's `states`, each with its `id`.
    pub fn from_report(report: &serde_json::Value) -> Option<Self> {
        let mut layout = Self::default();
        for (name, block) in report.as_object()? {
            let ids: Option<Vec<u64>> = block["states"]
                .as_array()?
                .iter()
                .map(|s| s["id"].as_u64())
                .collect();
            let ids = ids?;
            let first = *ids.iter().min()?;
            let count = ids.len() as u64;
            // A block's states are consecutive (`IdMapper.add` in order).
            let mut sorted = ids.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if sorted.len() as u64 != count || sorted.last()? + 1 != first + count {
                return None;
            }
            layout.insert(name, u32::try_from(first).ok()?, u32::try_from(count).ok()?);
        }
        Some(layout)
    }
    fn insert(&mut self, name: &str, first: u32, count: u32) {
        // A span past `u32::MAX` would name states no id can reach.
        let count = count.min(u32::MAX - first);
        self.blocks.insert(name.to_owned(), Span { first, count });
        self.total = self.total.max(first.saturating_add(count));
    }
    pub fn get(&self, block: &str) -> Option<Span> {
        self.blocks.get(block).copied()
    }
    /// How many vanilla states Horizon has.
    pub fn total(&self) -> u32 {
        self.total
    }
}

/// A run of consecutive server state ids that are consecutive Horizon ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    server: u32,
    horizon: u32,
    count: u32,
}

/// The server's block state ids, rebuilt from its synced block registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerStates {
    runs: Vec<Run>,
    /// The first state of the first block Horizon has no count for: from it
    /// on, every id is a mod's state.
    modded_from: u32,
    /// Those blocks, in raw id order.
    modded: Vec<String>,
}

impl ServerStates {
    /// `StateIdTracker.recalcStateMap` over the server's `minecraft:block`
    /// entries (name, raw id). Refused, with the namespaces involved, when a
    /// block Horizon knows follows one whose state count it lacks, or when
    /// raw ids repeat or are negative.
    pub fn rebuild(server_blocks: &[(String, i32)], layout: &Layout) -> Result<Self, Vec<String>> {
        let mut sorted: Vec<(i32, &str)> = server_blocks
            .iter()
            .map(|(name, id)| (*id, name.as_str()))
            .collect();
        sorted.sort_unstable();
        let mut bad = Vec::new();
        for pair in sorted.windows(2) {
            if pair[0].0 == pair[1].0 {
                bad.push(namespace_of(pair[1].1).to_owned());
            }
        }
        if let Some((id, name)) = sorted.first()
            && *id < 0
        {
            bad.push(namespace_of(name).to_owned());
        }
        let mut runs: Vec<Run> = Vec::new();
        let mut modded: Vec<String> = Vec::new();
        let mut next = 0u32;
        for (_, name) in &sorted {
            match layout.get(name) {
                // Its first state sits after the unknown blocks' states.
                Some(_) if !modded.is_empty() => {
                    bad.extend(modded.iter().map(|m| namespace_of(m).to_owned()));
                }
                Some(span) => {
                    match runs.last_mut() {
                        Some(run)
                            if u64::from(run.server) + u64::from(run.count) == u64::from(next)
                                && u64::from(run.horizon) + u64::from(run.count)
                                    == u64::from(span.first) =>
                        {
                            run.count += span.count
                        }
                        _ => runs.push(Run {
                            server: next,
                            horizon: span.first,
                            count: span.count,
                        }),
                    }
                    let Some(end) = next.checked_add(span.count) else {
                        bad.push(namespace_of(name).to_owned());
                        break;
                    };
                    next = end;
                }
                None => modded.push((*name).to_owned()),
            }
        }
        if !bad.is_empty() {
            bad.sort();
            bad.dedup();
            return Err(bad);
        }
        Ok(Self {
            runs,
            modded_from: next,
            modded,
        })
    }
    /// The Horizon state a server state id names; `None` for a mod's state
    /// (or one past the server's table), which the caller shows as a
    /// placeholder.
    pub fn horizon(&self, server: u32) -> Option<u32> {
        let i = self.runs.partition_point(|r| r.server <= server);
        let run = self.runs.get(i.checked_sub(1)?)?;
        (server - run.server < run.count).then(|| run.horizon + (server - run.server))
    }
    /// Every vanilla state keeps Horizon's id: the server's table starts
    /// with Horizon's whole vanilla table, in order.
    pub fn is_identity(&self, layout: &Layout) -> bool {
        match self.runs.as_slice() {
            [] => layout.total() == 0,
            [run] => run.server == 0 && run.horizon == 0 && run.count == layout.total(),
            _ => false,
        }
    }
    /// The first server state id that is a mod's.
    pub fn modded_from(&self) -> u32 {
        self.modded_from
    }
    /// The blocks whose states start at [`Self::modded_from`], in raw id order.
    pub fn modded(&self) -> &[String] {
        &self.modded
    }
}

/// Whether a strict registry's mod entries leave every vanilla id (and so,
/// for `minecraft:block`, every vanilla state id) where Horizon has it,
/// without knowing anything of the mods: each of Horizon's entries is on the
/// server at its own id, and each of the mods' comes after the last of them.
/// Then the server's raw id order starts with vanilla's whole order, and
/// `recalcStateMap` adds vanilla's states first, at the ids Horizon has,
/// whatever the mods' state counts. `Err` holds the namespaces that break it.
pub fn appended(known: &Ids, entries: &[(String, i32)]) -> Result<(), Vec<String>> {
    let last = known.names.len() as i64 - 1;
    let mut bad = Vec::new();
    let mut seen = 0usize;
    for (name, id) in entries {
        match known.id(name) {
            Some(ours) if ours == *id => seen += 1,
            Some(_) => bad.push(namespace_of(name).to_owned()),
            None if i64::from(*id) <= last => bad.push(namespace_of(name).to_owned()),
            None => {}
        }
    }
    // A vanilla entry the server lacks: a `REMOTE` remap moves it after the
    // server's entries, and every state after its own moves down.
    let vanilla = known.names.iter().filter(|n| !n.is_empty()).count();
    if seen < vanilla {
        bad.push("minecraft".to_owned());
    }
    if bad.is_empty() {
        Ok(())
    } else {
        bad.sort();
        bad.dedup();
        Err(bad)
    }
}

/// 26.3's block state count: every vanilla block's states, in registry
/// order (the adapter's `PINNED_STATES`; `Layout::from_report` counts the
/// same from the block report).
pub const VANILLA_STATES: u32 = 35723;

/// What a server mod's block state shows as where a block must stand (a
/// chunk, a block update): vanilla's stone, the facts
/// `horizon_items::blocks::unknown_state` gives an unknown id.
pub const PLACEHOLDER_STATE: i32 = 1;

/// Where a Fabric server's mod entries start in the registries Horizon reads
/// by raw id inside packets, once [`appended`] holds for each: vanilla's ids
/// come first and are Horizon's, every id from the bound on is a mod's. A
/// registry the server's mods add nothing to has no bound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub block_states: Option<i32>,
    pub items: Option<i32>,
    pub components: Option<i32>,
    pub particles: Option<i32>,
}

impl Bounds {
    /// The bounds a registry sync sets, `None` when the server's mods add
    /// nothing Horizon reads by raw id. Call it after [`super::remap`]
    /// accepted the sync: the vanilla entries are then where Horizon has them.
    pub fn from_sync(builtin: &Builtin, synced: &[super::SyncedRegistry]) -> Option<Self> {
        let mut bounds = Self::default();
        for registry in synced {
            let Some(known) = builtin.get(&registry.id) else {
                continue;
            };
            if registry
                .entries
                .iter()
                .all(|(name, _)| known.id(name).is_some())
            {
                continue;
            }
            let first = i32::try_from(known.names.len()).ok();
            match registry.id.as_str() {
                "minecraft:block" => bounds.block_states = i32::try_from(VANILLA_STATES).ok(),
                "minecraft:item" => bounds.items = first,
                "minecraft:data_component_type" => bounds.components = first,
                "minecraft:particle_type" => bounds.particles = first,
                _ => {}
            }
        }
        (bounds != Self::default()).then_some(bounds)
    }
}

thread_local! {
    /// The bounds of the server whose packets this thread decodes.
    static BOUNDS: Cell<Option<Bounds>> = const { Cell::new(None) };
}

/// The bounds packets on this thread are read with.
pub fn active() -> Option<Bounds> {
    BOUNDS.get()
}

/// Read this thread's packets with `bounds` from now on (a connection's net
/// thread, once its registry sync is in).
pub fn set(bounds: Option<Bounds>) {
    BOUNDS.set(bounds);
}

/// A connection's bounds: none while it lasts until [`set`], the previous
/// ones again when it ends.
pub struct Scope(Option<Bounds>);
impl Scope {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(BOUNDS.replace(None))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        BOUNDS.set(self.0);
    }
}

fn past(id: i32, bound: impl Fn(&Bounds) -> Option<i32>) -> bool {
    BOUNDS
        .get()
        .and_then(|b| bound(&b))
        .is_some_and(|first| id >= first)
}

/// An item id as read: -1 (no item Horizon has) for a server mod's.
pub fn item(id: i32) -> i32 {
    if past(id, |b| b.items) { -1 } else { id }
}
/// A data component type id as read: -1 for a server mod's, whose value no
/// reader can skip (it has no length), so the stack holding it fails to read.
pub fn component(id: i32) -> i32 {
    if past(id, |b| b.components) { -1 } else { id }
}
/// A particle type id as read: -1 for a server mod's.
pub fn particle(id: i32) -> i32 {
    if past(id, |b| b.particles) { -1 } else { id }
}
/// A block state id as read: -1 (none) for a server mod's.
pub fn block_state(id: i32) -> i32 {
    if past(id, |b| b.block_states) { -1 } else { id }
}
/// A block state id where a block must stand: [`PLACEHOLDER_STATE`] for a
/// server mod's. `first` is the server's first mod state, carried where the
/// thread's bounds don't reach (chunk decoding on worker threads).
pub fn placed_state(id: u32, first: Option<u32>) -> u32 {
    if first.is_some_and(|first| id >= first) {
        PLACEHOLDER_STATE as u32
    } else {
        id
    }
}

fn namespace_of(name: &str) -> &str {
    name.split_once(':').map_or("minecraft", |(ns, _)| ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synced(id: &str, names: &[&str]) -> super::super::SyncedRegistry {
        super::super::SyncedRegistry {
            id: id.into(),
            optional: false,
            entries: names
                .iter()
                .enumerate()
                .map(|(i, n)| ((*n).to_owned(), i as i32))
                .collect(),
        }
    }

    fn small_builtin() -> Builtin {
        Builtin::parse(
            r#"{
                "minecraft:block": {"entries": {
                    "minecraft:air": {"protocol_id": 0},
                    "minecraft:stone": {"protocol_id": 1}
                }},
                "minecraft:item": {"entries": {
                    "minecraft:air": {"protocol_id": 0},
                    "minecraft:stone": {"protocol_id": 1},
                    "minecraft:diamond": {"protocol_id": 2}
                }},
                "minecraft:data_component_type": {"entries": {
                    "minecraft:custom_data": {"protocol_id": 0}
                }},
                "minecraft:particle_type": {"entries": {
                    "minecraft:angry_villager": {"protocol_id": 0}
                }}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn bounds_start_where_the_mods_entries_do() {
        let builtin = small_builtin();
        // Nothing of the mods' where packets carry raw ids: no bounds.
        let plain = [synced(
            "minecraft:item",
            &["minecraft:air", "minecraft:stone", "minecraft:diamond"],
        )];
        assert_eq!(Bounds::from_sync(&builtin, &plain), None);
        let modded = [
            synced(
                "minecraft:block",
                &["minecraft:air", "minecraft:stone", "copper_age:vent"],
            ),
            synced(
                "minecraft:item",
                &[
                    "minecraft:air",
                    "minecraft:stone",
                    "minecraft:diamond",
                    "copper_age:wrench",
                ],
            ),
            synced(
                "minecraft:data_component_type",
                &["minecraft:custom_data", "copper_age:charge"],
            ),
            synced("minecraft:particle_type", &["minecraft:angry_villager"]),
        ];
        assert_eq!(
            Bounds::from_sync(&builtin, &modded),
            Some(Bounds {
                // Every vanilla block's states come first, whatever the mods'.
                block_states: Some(35723),
                items: Some(3),
                components: Some(1),
                particles: None,
            })
        );
    }

    #[test]
    fn ids_past_a_bound_read_as_unknown_on_the_connections_thread() {
        let bounds = Bounds {
            block_states: Some(35723),
            items: Some(1012),
            components: Some(100),
            particles: Some(120),
        };
        assert_eq!(item(1012), 1012);
        {
            let _connection = Scope::new();
            set(Some(bounds));
            assert_eq!((item(1011), item(1012)), (1011, -1));
            assert_eq!((component(99), component(100)), (99, -1));
            assert_eq!((particle(119), particle(120)), (119, -1));
            assert_eq!((block_state(35722), block_state(35723)), (35722, -1));
            // Through the readers' own path (`adapter::ids`).
            assert_eq!(crate::adapter::ids::block_state(40000), -1);
            assert_eq!(crate::adapter::ids::item(5), 5);
            {
                let _next = Scope::new();
                assert_eq!(item(1012), 1012);
            }
            assert_eq!(item(1012), -1);
        }
        assert_eq!(item(1012), 1012);
    }

    #[test]
    fn a_stack_of_a_mod_item_has_no_item_and_a_mod_component_stops_the_read() {
        use crate::buf::{Codec, Reader};
        use crate::item::Slot;
        let _connection = Scope::new();
        set(Some(Bounds {
            items: Some(1012),
            components: Some(100),
            ..Bounds::default()
        }));
        // ItemStack.OPTIONAL_STREAM_CODEC: count 2, item 1012 (VarInt f4 07),
        // no components added or removed.
        let stack = Slot::read(&mut Reader::new(&[2, 0xf4, 0x07, 0, 0]))
            .unwrap()
            .unwrap();
        assert_eq!((stack.item, stack.count), (-1, 2));
        // Count 1, item 5, one component added: type 100 (VarInt 64), whose
        // value has no length to skip it by.
        assert!(Slot::read(&mut Reader::new(&[1, 5, 1, 0, 0x64, 0x01])).is_err());
    }

    #[test]
    fn a_mod_particle_is_an_error_not_a_native_mods_or_a_dataless_one() {
        use crate::buf::{Reader, Writer};
        use crate::entity_data::{Particle, ParticleData};
        let _connection = Scope::new();
        set(Some(Bounds {
            particles: Some(120),
            ..Bounds::default()
        }));
        // Type 120 (VarInt 78) and two bytes of a shape only its mod knows.
        assert!(Particle::read(&mut Reader::new(&[0x78, 0x01, 0x02])).is_err());
        // Horizon's own native-mod sentinel still reads its name.
        let mut w = Writer::new();
        w.varint(-1);
        w.string("starfall:spark");
        let native = Particle::read(&mut Reader::new(&w.data)).unwrap();
        assert_eq!(
            native.data,
            Some(ParticleData::Mod("starfall:spark".into()))
        );
    }

    #[test]
    fn a_placed_mod_state_is_stone() {
        assert_eq!(placed_state(35722, Some(35723)), 35722);
        assert_eq!(placed_state(35723, Some(35723)), 1);
        assert_eq!(placed_state(u32::MAX, Some(35723)), 1);
        assert_eq!(placed_state(40000, None), 40000);
    }

    #[test]
    fn vanilla_states_match_the_block_report() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/datagen/out/reports/blocks.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let report: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            Layout::from_report(&report).unwrap().total(),
            VANILLA_STATES
        );
    }

    /// The first 14 blocks of 26.3's block report
    /// (`.local/datagen/out/reports/blocks.json`), copied by hand:
    /// grass_block and podzol have two states (`snowy`), the rest one.
    fn layout() -> Layout {
        let mut layout = Layout::default();
        for (name, first, count) in [
            ("minecraft:air", 0, 1),
            ("minecraft:stone", 1, 1),
            ("minecraft:granite", 2, 1),
            ("minecraft:polished_granite", 3, 1),
            ("minecraft:diorite", 4, 1),
            ("minecraft:polished_diorite", 5, 1),
            ("minecraft:andesite", 6, 1),
            ("minecraft:polished_andesite", 7, 1),
            ("minecraft:grass_block", 8, 2),
            ("minecraft:dirt", 10, 1),
            ("minecraft:coarse_dirt", 11, 1),
            ("minecraft:podzol", 12, 2),
            ("minecraft:cobblestone", 14, 1),
            ("minecraft:oak_planks", 15, 1),
        ] {
            layout.insert(name, first, count);
        }
        layout
    }

    /// Those blocks at their `registries.json` protocol ids (0 to 13).
    fn vanilla() -> Vec<(String, i32)> {
        [
            "air",
            "stone",
            "granite",
            "polished_granite",
            "diorite",
            "polished_diorite",
            "andesite",
            "polished_andesite",
            "grass_block",
            "dirt",
            "coarse_dirt",
            "podzol",
            "cobblestone",
            "oak_planks",
        ]
        .iter()
        .enumerate()
        .map(|(i, n)| (format!("minecraft:{n}"), i as i32))
        .collect()
    }

    #[test]
    fn mod_blocks_at_the_end_keep_every_vanilla_state() {
        let mut server = vanilla();
        server.push(("copper_age:vent".into(), 14));
        server.push(("copper_age:pipe".into(), 15));
        let states = ServerStates::rebuild(&server, &layout()).unwrap();
        assert!(states.is_identity(&layout()));
        // podzol[snowy=false] is 13 in the report, oak_planks 15.
        assert_eq!(states.horizon(13), Some(13));
        assert_eq!(states.horizon(15), Some(15));
        // Vanilla's 16 states end at 15: 16 on is the mods'.
        assert_eq!(states.modded_from(), 16);
        assert_eq!(states.horizon(16), None);
        assert_eq!(states.horizon(40_000), None);
        assert_eq!(states.modded(), ["copper_age:vent", "copper_age:pipe"]);
    }

    #[test]
    fn raw_id_order_not_payload_order_and_gaps_take_no_states() {
        // `Int2ObjectRBTreeMap`: sorted by raw id; the gap at 2..=9 is no state.
        let server = vec![
            ("minecraft:grass_block".into(), 10),
            ("minecraft:air".into(), 0),
            ("minecraft:dirt".into(), 11),
            ("minecraft:stone".into(), 1),
        ];
        let states = ServerStates::rebuild(&server, &layout()).unwrap();
        // Server: air 0, stone 1, grass_block 2 and 3, dirt 4.
        assert_eq!(states.horizon(0), Some(0));
        assert_eq!(states.horizon(1), Some(1));
        assert_eq!(states.horizon(2), Some(8));
        assert_eq!(states.horizon(3), Some(9));
        assert_eq!(states.horizon(4), Some(10));
        assert_eq!(states.horizon(5), None);
        assert!(!states.is_identity(&layout()));
    }

    #[test]
    fn a_missing_vanilla_block_shifts_the_states_after_it() {
        // No granite (state 2): polished_granite's 3 is the server's 2,
        // podzol[snowy=true] 12 is 11, oak_planks 15 is 14.
        let server: Vec<_> = vanilla()
            .into_iter()
            .filter(|(n, _)| n != "minecraft:granite")
            .collect();
        let states = ServerStates::rebuild(&server, &layout()).unwrap();
        assert_eq!(states.horizon(1), Some(1));
        assert_eq!(states.horizon(2), Some(3));
        assert_eq!(states.horizon(11), Some(12));
        assert_eq!(states.horizon(14), Some(15));
        assert_eq!(states.modded_from(), 15);
        assert!(!states.is_identity(&layout()));
    }

    #[test]
    fn a_known_block_after_a_mod_block_is_refused() {
        let mut server = vanilla();
        server[13].1 = 20; // oak_planks after the mod's block
        server.push(("copper_age:vent".into(), 14));
        assert_eq!(
            ServerStates::rebuild(&server, &layout()),
            Err(vec!["copper_age".into()])
        );
    }

    #[test]
    fn repeated_and_negative_raw_ids_are_refused() {
        let mut server = vanilla();
        server.push(("copper_age:vent".into(), 3));
        assert!(ServerStates::rebuild(&server, &layout()).is_err());
        let mut server = vanilla();
        server.push(("copper_age:vent".into(), -1));
        assert!(ServerStates::rebuild(&server, &layout()).is_err());
        assert!(ServerStates::rebuild(&[], &layout()).is_ok());
    }

    #[test]
    fn huge_state_counts_never_overflow() {
        let mut layout = Layout::default();
        layout.insert("minecraft:a", 0, u32::MAX);
        layout.insert("minecraft:b", 0, u32::MAX);
        let server = vec![("minecraft:a".into(), 0), ("minecraft:b".into(), 1)];
        assert!(ServerStates::rebuild(&server, &layout).is_err());
        let states = ServerStates::rebuild(&server[..1], &layout).unwrap();
        assert_eq!(states.horizon(u32::MAX), None);
        assert_eq!(states.horizon(u32::MAX - 1), Some(u32::MAX - 1));
    }

    fn ids() -> Ids {
        Ids::from_names(vanilla().into_iter().map(|(n, _)| n).collect())
    }

    #[test]
    fn appended_entries_are_proven_without_state_counts() {
        let mut server = vanilla();
        server.push(("copper_age:vent".into(), 14));
        server.push(("copper_age:pipe".into(), 30));
        assert_eq!(appended(&ids(), &server), Ok(()));
        // A mod's entry inside vanilla's range: a vanilla one is gone.
        let mut inside = vanilla();
        inside[5] = ("copper_age:vent".into(), 5);
        assert_eq!(
            appended(&ids(), &inside),
            Err(vec!["copper_age".into(), "minecraft".into()])
        );
        // A vanilla entry the server doesn't have.
        let mut missing = vanilla();
        missing.pop();
        assert_eq!(appended(&ids(), &missing), Err(vec!["minecraft".into()]));
    }

    /// The whole 26.3 block table, when the data generator's reports are
    /// there (`.local/datagen/out/reports`): with a mod block after vanilla's
    /// the server keeps all 35723 vanilla state ids (0 to 35722 in the
    /// report, firefly_bush last).
    #[test]
    fn the_vanilla_report_keeps_its_ids() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/datagen/out/reports");
        let (Ok(blocks), Ok(registries)) = (
            std::fs::read_to_string(root.join("blocks.json")),
            std::fs::read_to_string(root.join("registries.json")),
        ) else {
            eprintln!("no data generator reports: skipped");
            return;
        };
        let report: serde_json::Value = serde_json::from_str(&blocks).unwrap();
        let layout = Layout::from_report(&report).unwrap();
        assert_eq!(layout.total(), 35723);
        assert_eq!(
            layout.get("minecraft:acacia_button"),
            Some(Span {
                first: 12514,
                count: 24
            })
        );
        let builtin = crate::registry::Builtin::parse(&registries).unwrap();
        let known = builtin.get("minecraft:block").unwrap();
        let mut server: Vec<(String, i32)> = known
            .names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i as i32))
            .collect();
        assert_eq!(server.len(), 1286);
        server.push(("copper_age:vent".into(), 1286));
        assert_eq!(appended(known, &server), Ok(()));
        let states = ServerStates::rebuild(&server, &layout).unwrap();
        assert!(states.is_identity(&layout));
        assert_eq!(states.modded_from(), 35723);
        assert_eq!(states.horizon(35722), Some(35722));
        assert_eq!(states.horizon(35723), None);
        // Without stone (state 1), firefly_bush's 35722 is the server's 35721.
        server.retain(|(n, _)| n != "minecraft:stone");
        let states = ServerStates::rebuild(&server, &layout).unwrap();
        assert_eq!(states.horizon(35721), Some(35722));
        assert_eq!(states.horizon(1), Some(2));
    }
}
