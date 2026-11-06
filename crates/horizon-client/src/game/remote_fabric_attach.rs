//! Fabric API's synced data attachments (`fabric:attachment_sync_v1`) as a
//! client of a Fabric server keeps them: the value bytes of each attachment
//! a native mod accepted (`horizon_protocol::fabric::attachment::accept`), by
//! target and attachment id, for the mod to decode with its own codec.
//!
//! A Fabric client hangs the values on the objects themselves, so they go
//! when those go (`AttachmentSyncClient`, `AttachmentChange.tryApply`): an
//! entity's on `RemoveEntities` or when another entity takes its id; a block
//! entity's when its block changes or its column is sent again
//! (`LevelChunk.replaceWithPacketData` clears the block entities, the server
//! then syncs them anew); a chunk's and its block entities' on
//! `ForgetLevelChunk`; the level's, the chunks' and the other entities' when a
//! respawn changes dimension (a new `ClientLevel`); everything on `Login` (a
//! new `ClientPacketListener`, with its `GlobalAttachments`). The own player
//! keeps its values through a respawn, after a death only those whose type
//! copies on death (`ClientPacketListenerMixin.copyAttachmentsOnClientRespawn`).
use super::Game;
use horizon_protocol::fabric::attachment::{self as fa, Accepted, Change, Target};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;

/// Bytes of values kept at most: a server can't fill memory with distinct
/// targets. Fabric keeps no such total; past it new values are dropped.
pub(super) const MAX_STORED_BYTES: usize = 64 << 20;

/// The values a Fabric server synced, by target and attachment id.
#[derive(Default, Debug)]
pub(super) struct FabricAttachments {
    values: HashMap<Target, HashMap<String, Vec<u8>>>,
    bytes: usize,
}

impl FabricAttachments {
    /// One `fabric:attachment_sync_v1` payload, decoded against the types
    /// Horizon accepted.
    pub(super) fn decode(data: &[u8], accepted: &[Accepted]) -> Result<Change, String> {
        fa::decode_change(data, |id| {
            accepted.iter().find(|a| a.id == id).map(|a| a.max_size)
        })
        .map_err(|e| e.0)
    }

    /// `target.setAttached(type, value)`: a value replaces the one before, no
    /// value removes it.
    pub(super) fn apply(&mut self, change: Change) {
        let Change { target, id, value } = change;
        let old = self
            .values
            .get(&target)
            .and_then(|m| m.get(&id))
            .map_or(0, Vec::len);
        match value {
            Some(value) => {
                if self.bytes - old + value.len() > MAX_STORED_BYTES {
                    log::debug!("Horizon: Fabric attachment {id} dropped, the store is full");
                    return;
                }
                self.bytes = self.bytes - old + value.len();
                self.values.entry(target).or_default().insert(id, value);
            }
            None => {
                if let Some(map) = self.values.get_mut(&target) {
                    map.remove(&id);
                    self.bytes -= old;
                    if map.is_empty() {
                        self.values.remove(&target);
                    }
                }
            }
        }
    }

    /// `AttachmentTarget.getAttached`: the value's bytes as the type's
    /// `StreamCodec` wrote them.
    pub(super) fn get(&self, target: Target, id: &str) -> Option<&[u8]> {
        self.values.get(&target)?.get(id).map(Vec::as_slice)
    }

    fn retain(&mut self, keep: impl Fn(&Target) -> bool) {
        let mut freed = 0;
        self.values.retain(|target, map| {
            let k = keep(target);
            if !k {
                freed += map.values().map(Vec::len).sum::<usize>();
            }
            k
        });
        self.bytes -= freed;
    }

    /// The entity with this network id is gone, or another took its id.
    pub(super) fn forget_entity(&mut self, id: i32) {
        self.retain(|t| *t != Target::Entity(id));
    }

    /// The block entity at `pos` is gone or replaced.
    pub(super) fn forget_block_entity(&mut self, pos: [i32; 3]) {
        self.retain(|t| *t != Target::BlockEntity(pos));
    }

    /// The column was sent (again): its block entities are new ones. The
    /// chunk keeps its own values, as the client's `LevelChunk` stays.
    pub(super) fn column_received(&mut self, x: i32, z: i32) {
        self.retain(
            |t| !matches!(*t, Target::BlockEntity([bx, _, bz]) if (bx >> 4, bz >> 4) == (x, z)),
        );
    }

    /// The columns were unloaded: their chunks and block entities go.
    pub(super) fn forget_columns<S: BuildHasher>(&mut self, gone: &HashSet<(i32, i32), S>) {
        self.retain(|t| match *t {
            Target::Chunk([x, z]) => !gone.contains(&(x, z)),
            Target::BlockEntity([x, _, z]) => !gone.contains(&(x >> 4, z >> 4)),
            _ => true,
        });
    }

    /// `handleRespawn`: a new `LocalPlayer` under the old id takes the old
    /// one's values (`AttachmentTargetImpl.transfer`), all of them unless it
    /// died (no `KEEP_ATTRIBUTE_MODIFIERS`), then only the copy-on-death
    /// ones. A new dimension is a new level: the level, its chunks, block
    /// entities and other entities lose theirs. Global ones stay.
    pub(super) fn respawn(
        &mut self,
        own_id: i32,
        dimension_changed: bool,
        keep_attributes: bool,
        accepted: &[Accepted],
    ) {
        if !keep_attributes && let Some(map) = self.values.get_mut(&Target::Entity(own_id)) {
            let before: usize = map.values().map(Vec::len).sum();
            map.retain(|id, _| accepted.iter().any(|a| a.id == *id && a.copy_on_death));
            let after: usize = map.values().map(Vec::len).sum();
            self.bytes -= before - after;
        }
        if dimension_changed {
            self.retain(|t| matches!(*t, Target::Global) || *t == Target::Entity(own_id));
        }
    }

    /// `handleLogin` or a disconnect: a new connection listener.
    pub(super) fn clear(&mut self) {
        self.values.clear();
        self.bytes = 0;
    }
}

impl Game<'_> {
    /// A `fabric:attachment_sync_v1` payload. A target the client doesn't
    /// have is skipped with a log line, as `tryApply` warns (Fabric
    /// disconnects only with `fabric.attachment.disconnect_on_unknown_targets`);
    /// a payload that doesn't decode is skipped too, where Fabric's decoder
    /// would end the connection.
    pub(super) fn remote_fabric_attachment_sync(&mut self, data: &[u8]) {
        let change = match FabricAttachments::decode(data, &fa::accepted()) {
            Ok(change) => change,
            Err(e) => return log::debug!("Horizon: a bad Fabric attachment sync: {e}"),
        };
        if !self.fabric_target_exists(change.target) {
            return log::debug!(
                "Horizon: Fabric attachment {} for {:?}, which the client doesn't have",
                change.id,
                change.target
            );
        }
        if let Some(r) = self.remote.as_mut() {
            r.fabric_attachments.apply(change);
        }
    }

    /// `AttachmentTargetInfo.getTarget(level)` is not null.
    fn fabric_target_exists(&self, target: Target) -> bool {
        match target {
            Target::Entity(id) => {
                self.remote.as_ref().is_some_and(|r| r.entity_id == id)
                    || self.persist.uuids.contains_key(&(id as u32))
            }
            Target::BlockEntity([x, y, z]) => self.world.block_entities.contains_key(&(x, y, z)),
            Target::Chunk([x, z]) => self.world.chunks.contains_key(&(x, z)),
            Target::World | Target::Global => true,
        }
    }

    /// The read API for native mods: the bytes a Fabric server synced for
    /// the attachment `id` on `target`, `None` when it sent none (or Horizon
    /// is not a client of a Fabric server). The own player is
    /// `Target::Entity` of its network id.
    #[allow(dead_code)] // Until the `fabric_attachment` helper calls it.
    pub(super) fn fabric_attachment(&self, target: Target, id: &str) -> Option<&[u8]> {
        self.remote.as_ref()?.fabric_attachments.get(target, id)
    }

    /// `handleRespawn`'s part (`keep` is the packet's flags).
    pub(super) fn fabric_attachments_respawn(&mut self, dimension_changed: bool, keep: i8) {
        let accepted = fa::accepted();
        if let Some(r) = self.remote.as_mut() {
            let own = r.entity_id;
            r.fabric_attachments
                .respawn(own, dimension_changed, keep & 1 != 0, &accepted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted() -> Vec<Accepted> {
        vec![
            Accepted {
                id: "mymod:mana".into(),
                max_size: 8,
                copy_on_death: false,
            },
            Accepted {
                id: "mymod:soul".into(),
                max_size: fa::DEFAULT_MAX_DATA_SIZE,
                copy_on_death: true,
            },
        ]
    }

    /// A change's bytes, written by hand as `AttachmentChange.encodePacket`
    /// lays them out: the target, the identifier, the value flag, the value.
    fn payload(target: &[u8], id: &str, value: Option<&[u8]>) -> Vec<u8> {
        let mut out = target.to_vec();
        out.push(id.len() as u8);
        out.extend_from_slice(id.as_bytes());
        match value {
            Some(v) => {
                out.push(1);
                out.extend_from_slice(v);
            }
            None => out.push(0),
        }
        out
    }

    fn apply(store: &mut FabricAttachments, data: &[u8]) {
        store.apply(FabricAttachments::decode(data, &accepted()).unwrap());
    }

    #[test]
    fn values_land_on_every_target_and_removal_clears_them() {
        let mut store = FabricAttachments::default();
        // Entity 7, block entity (1, 2, 3), chunk (1, 2), the level, global.
        let targets: [(&[u8], Target); 5] = [
            (&[1, 7], Target::Entity(7)),
            (
                &[0, 0, 0, 0, 0x40, 0, 0, 0x30, 0x02],
                Target::BlockEntity([1, 2, 3]),
            ),
            (&[2, 0x81, 0x80, 0x80, 0x80, 0x20], Target::Chunk([1, 2])),
            (&[3], Target::World),
            (&[4], Target::Global),
        ];
        for (bytes, target) in targets {
            apply(
                &mut store,
                &payload(bytes, "mymod:mana", Some(&[0, 0, 0, 9])),
            );
            assert_eq!(
                store.get(target, "mymod:mana"),
                Some(&[0, 0, 0, 9][..]),
                "{target:?}"
            );
        }
        assert_eq!(store.bytes, 20);
        apply(&mut store, &payload(&[3], "mymod:mana", None));
        assert_eq!(store.get(Target::World, "mymod:mana"), None);
        assert_eq!(store.bytes, 16);
    }

    #[test]
    fn unaccepted_oversized_and_cut_payloads_are_refused() {
        let acc = accepted();
        assert!(FabricAttachments::decode(&payload(&[3], "other:x", Some(&[1])), &acc).is_err());
        assert!(
            FabricAttachments::decode(&payload(&[3], "mymod:mana", Some(&[0; 9])), &acc).is_err()
        );
        let full = payload(&[1, 7], "mymod:mana", Some(&[1, 2]));
        for len in 0..full.len() - 2 {
            assert!(
                FabricAttachments::decode(&full[..len], &acc).is_err(),
                "{len}"
            );
        }
    }

    #[test]
    fn a_removed_entity_takes_its_values_along() {
        let mut store = FabricAttachments::default();
        apply(&mut store, &payload(&[1, 7], "mymod:mana", Some(&[1])));
        apply(&mut store, &payload(&[1, 7], "mymod:soul", Some(&[2])));
        apply(&mut store, &payload(&[1, 8], "mymod:mana", Some(&[3])));
        store.forget_entity(7);
        assert_eq!(store.get(Target::Entity(7), "mymod:mana"), None);
        assert_eq!(store.get(Target::Entity(7), "mymod:soul"), None);
        assert_eq!(store.get(Target::Entity(8), "mymod:mana"), Some(&[3][..]));
        assert_eq!(store.bytes, 1);
    }

    #[test]
    fn columns_drop_their_chunk_and_block_entities() {
        let mut store = FabricAttachments::default();
        // Block entity (1, 2, 3) is in column (0, 0); chunk (1, 2).
        apply(
            &mut store,
            &payload(
                &[0, 0, 0, 0, 0x40, 0, 0, 0x30, 0x02],
                "mymod:mana",
                Some(&[1]),
            ),
        );
        apply(&mut store, &payload(&[2, 0], "mymod:mana", Some(&[2])));
        apply(
            &mut store,
            &payload(&[2, 0x81, 0x80, 0x80, 0x80, 0x20], "mymod:mana", Some(&[3])),
        );
        // Sent again: the chunk keeps its value, the block entity loses it.
        store.column_received(0, 0);
        assert_eq!(
            store.get(Target::BlockEntity([1, 2, 3]), "mymod:mana"),
            None
        );
        assert_eq!(
            store.get(Target::Chunk([0, 0]), "mymod:mana"),
            Some(&[2][..])
        );
        store.forget_columns(&[(0, 0)].into_iter().collect::<HashSet<_>>());
        assert_eq!(store.get(Target::Chunk([0, 0]), "mymod:mana"), None);
        assert_eq!(
            store.get(Target::Chunk([1, 2]), "mymod:mana"),
            Some(&[3][..])
        );
    }

    #[test]
    fn a_respawn_keeps_what_the_new_player_and_level_keep() {
        let fill = |store: &mut FabricAttachments| {
            apply(store, &payload(&[1, 7], "mymod:mana", Some(&[1])));
            apply(store, &payload(&[1, 7], "mymod:soul", Some(&[2])));
            apply(store, &payload(&[1, 8], "mymod:mana", Some(&[3])));
            apply(store, &payload(&[3], "mymod:mana", Some(&[4])));
            apply(store, &payload(&[4], "mymod:mana", Some(&[5])));
        };
        // Death in the same dimension: the own player keeps soul only.
        let mut store = FabricAttachments::default();
        fill(&mut store);
        store.respawn(7, false, false, &accepted());
        assert_eq!(store.get(Target::Entity(7), "mymod:mana"), None);
        assert_eq!(store.get(Target::Entity(7), "mymod:soul"), Some(&[2][..]));
        assert_eq!(store.get(Target::Entity(8), "mymod:mana"), Some(&[3][..]));
        assert_eq!(store.get(Target::World, "mymod:mana"), Some(&[4][..]));
        // Through a portal: the player keeps all, the level and others go.
        let mut store = FabricAttachments::default();
        fill(&mut store);
        store.respawn(7, true, true, &accepted());
        assert_eq!(store.get(Target::Entity(7), "mymod:mana"), Some(&[1][..]));
        assert_eq!(store.get(Target::Entity(8), "mymod:mana"), None);
        assert_eq!(store.get(Target::World, "mymod:mana"), None);
        assert_eq!(store.get(Target::Global, "mymod:mana"), Some(&[5][..]));
        assert_eq!(store.bytes, 3);
        store.clear();
        assert_eq!(store.get(Target::Global, "mymod:mana"), None);
    }

    #[test]
    fn the_store_has_a_ceiling() {
        let mut store = FabricAttachments::default();
        let big = vec![0; fa::DEFAULT_MAX_DATA_SIZE];
        for id in 0..70 {
            store.apply(Change {
                target: Target::Entity(id),
                id: "mymod:soul".into(),
                value: Some(big.clone()),
            });
        }
        assert!(store.bytes <= MAX_STORED_BYTES);
        assert_eq!(store.get(Target::Entity(69), "mymod:soul"), None);
        // Replacing a value counts only the difference.
        store.apply(Change {
            target: Target::Entity(0),
            id: "mymod:soul".into(),
            value: Some(vec![1]),
        });
        assert_eq!(store.get(Target::Entity(0), "mymod:soul"), Some(&[1][..]));
    }
}
