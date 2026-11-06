//! Fabric API data-attachment-api-v1: attachment sync (Fabric API 0.162,
//! `impl/attachment/sync/`).
//!
//! In configuration a Fabric server whose client registered
//! [`ACCEPTED_CHANNEL`] asks which synced attachment types the client knows
//! (`ClientboundRequestAcceptedAttachmentsPayload`, no body) and waits for the
//! answer (`ServerboundAcceptedAttachmentsPayload`: a set of identifiers). It
//! keeps the ids its own mods register as synced and sends only those, in play,
//! one `fabric:attachment_sync_v1` payload per change
//! (`ClientboundAttachmentSyncPayload` around an `AttachmentChange`): the
//! target, the attachment's identifier, whether a value follows, then the
//! value in the type's own `StreamCodec`, up to the end of the payload.
//!
//! The ids travel only from client to server: a client can't learn which
//! types a server syncs, so it names the ones it can use. Horizon names those
//! its native mods [`accept`]; it keeps their values as opaque bytes
//! (`game/remote_fabric_attach.rs`), since each value's codec is the mod's.
use crate::{
    Error, Result,
    buf::{Reader, Writer},
};
use std::sync::RwLock;

/// Both directions of the configuration exchange
/// (`ClientboundRequestAcceptedAttachmentsPayload.PACKET_ID`,
/// `ServerboundAcceptedAttachmentsPayload.PACKET_ID`).
pub const ACCEPTED_CHANNEL: &str = "fabric:accepted_attachments_v1";
/// `ClientboundAttachmentSyncPayload.PACKET_ID`, play, clientbound.
pub const SYNC_CHANNEL: &str = "fabric:attachment_sync_v1";
/// `ServerboundAcceptedAttachmentsPayload.MAX_ACCEPTED_ATTACHMENTS`.
pub const MAX_ACCEPTED: usize = 8192;
/// `AttachmentSync.MAX_IDENTIFIER_SIZE`: a synced type's id, in chars.
pub const MAX_IDENTIFIER_SIZE: usize = 256;
/// `AttachmentTargetInfo.MAX_SIZE_IN_BYTES`: the type byte and a long.
pub const MAX_TARGET_SIZE: usize = 1 + 8;
/// `AttachmentSync.MAX_PADDING_SIZE_IN_BYTES`.
pub const MAX_PADDING_SIZE: usize = MAX_TARGET_SIZE + MAX_IDENTIFIER_SIZE;
/// `ClientboundCustomPayloadPacket.MAX_PAYLOAD_SIZE`.
const MAX_PAYLOAD_SIZE: usize = 1_048_576;
/// `AttachmentSync.DEFAULT_MAX_DATA_SIZE`: the payload minus the padding
/// and the networking API's own (`VarInt.getByteSize(75) + 75 + 5 * 2`, 75
/// being `ByteBufUtil.utf8MaxBytes` of the 25-char channel), 1 048 225 bytes.
/// A type's `maxSyncSize` below this is raised to it.
pub const DEFAULT_MAX_DATA_SIZE: usize = MAX_PAYLOAD_SIZE - MAX_PADDING_SIZE - (1 + 25 * 3 + 10);
/// `AttachmentSync.DEFAULT_ATTACHMENT_SYNC_PACKET_SIZE`.
pub const DEFAULT_SYNC_PACKET_SIZE: usize = MAX_PADDING_SIZE + DEFAULT_MAX_DATA_SIZE;
/// `Identifier.STREAM_CODEC` reads `STRING_UTF8`, 32767 chars at most.
const MAX_STRING: usize = 32767;

/// `AttachmentTargetInfo`: what an attachment hangs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Type 0, `BlockEntityTarget`: `BlockPos.STREAM_CODEC`, a packed long.
    BlockEntity([i32; 3]),
    /// Type 1, `EntityTarget`: the entity's network id as a VarInt.
    Entity(i32),
    /// Type 2, `ChunkTarget`: `ChunkPos.pack` as a VarLong.
    Chunk([i32; 2]),
    /// Type 3, `LevelTarget`: the client's level, no fields.
    World,
    /// Type 4, `GlobalTarget`: `GlobalAttachments`, the connection's, no fields.
    Global,
}

/// `AttachmentChange`: a value set on a target, or removed (`None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub target: Target,
    /// The attachment type's identifier, `namespace:path`.
    pub id: String,
    /// The value as the type's `StreamCodec` wrote it.
    pub value: Option<Vec<u8>>,
}

/// `BlockPos.asLong`: x in the top 26 bits, z in the next 26, y in the low 12.
fn pack_block_pos([x, y, z]: [i32; 3]) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38)
        | ((i64::from(z) & 0x3FF_FFFF) << 12)
        | (i64::from(y) & 0xFFF)
}

fn unpack_block_pos(v: i64) -> [i32; 3] {
    [
        (v >> 38) as i32,
        (v << 52 >> 52) as i32,
        (v << 26 >> 38) as i32,
    ]
}

/// `AttachmentTargetInfo.PACKET_CODEC`: `ByteBufCodecs.BYTE.dispatch`, the
/// type byte, then its fields.
pub fn encode_target(w: &mut Writer, target: Target) {
    match target {
        Target::BlockEntity(pos) => {
            w.i8(0);
            w.i64(pack_block_pos(pos));
        }
        Target::Entity(id) => {
            w.i8(1);
            w.varint(id);
        }
        Target::Chunk([x, z]) => {
            w.i8(2);
            w.varlong(i64::from(x as u32) | (i64::from(z as u32) << 32));
        }
        Target::World => w.i8(3),
        Target::Global => w.i8(4),
    }
}

/// An unknown type byte is an error, as `Type.streamCodecFromId`'s null is.
pub fn decode_target(r: &mut Reader) -> Result<Target> {
    Ok(match r.i8()? {
        0 => Target::BlockEntity(unpack_block_pos(r.i64()?)),
        1 => Target::Entity(r.varint()?),
        2 => {
            let packed = r.varlong()?;
            Target::Chunk([packed as i32, (packed >> 32) as i32])
        }
        3 => Target::World,
        4 => Target::Global,
        t => {
            return Err(Error::new(format!(
                "attachment sync: unknown target type {t}"
            )));
        }
    })
}

/// `AttachmentChange.encodePacket`; an error for a value past `max_size`
/// (`validateMaxPayloadSize`).
pub fn encode_change(change: &Change, max_size: usize) -> Result<Vec<u8>> {
    let mut w = Writer::new();
    encode_target(&mut w, change.target);
    w.string(&change.id);
    match &change.value {
        None => w.bool(false),
        Some(value) => {
            if value.len() > max_size {
                return Err(too_big(&change.id, value.len(), max_size));
            }
            w.bool(true);
            w.bytes(value);
        }
    }
    Ok(w.data)
}

/// `AttachmentChange.decodePacket`: `max_size` gives an accepted type's
/// `maxSyncSize`, `None` for a type the client doesn't know (Fabric's
/// "attachment type cannot be null"). A value is the rest of the payload,
/// at most that size.
pub fn decode_change(data: &[u8], max_size: impl Fn(&str) -> Option<usize>) -> Result<Change> {
    let mut r = Reader::new(data);
    let target = decode_target(&mut r)?;
    let id = normalize(&r.string(MAX_STRING)?)
        .ok_or_else(|| Error::new("attachment sync: bad attachment identifier"))?;
    let max = max_size(&id)
        .ok_or_else(|| Error::new(format!("attachment sync: unknown attachment type {id}")))?;
    let value = if r.bool()? {
        let size = r.remaining();
        if size > max {
            return Err(too_big(&id, size, max));
        }
        Some(r.rest().to_vec())
    } else {
        None
    };
    Ok(Change { target, id, value })
}

fn too_big(id: &str, size: usize, max: usize) -> Error {
    Error::new(format!(
        "Data for attachment '{id}' was too big ({size} bytes, over maximum {max})"
    ))
}

/// `ServerboundAcceptedAttachmentsPayload.CODEC`:
/// `ByteBufCodecs.collection(HashSet::new, Identifier.STREAM_CODEC, 8192)`.
pub fn encode_accepted(ids: &[&str]) -> Result<Vec<u8>> {
    if ids.len() > MAX_ACCEPTED {
        return Err(Error::new("too many accepted attachments"));
    }
    let mut w = Writer::new();
    w.varint(ids.len() as i32);
    for id in ids {
        w.string(id);
    }
    Ok(w.data)
}

/// The server's side of [`encode_accepted`] (a Horizon server could ask too).
pub fn decode_accepted(data: &[u8]) -> Result<Vec<String>> {
    let mut r = Reader::new(data);
    let n = r.count(MAX_ACCEPTED)?;
    let mut ids = Vec::with_capacity(n.min(r.remaining()));
    for _ in 0..n {
        let id = normalize(&r.string(MAX_STRING)?)
            .ok_or_else(|| Error::new("accepted attachments: bad identifier"))?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if r.remaining() > 0 {
        return Err(Error::new("accepted attachments: trailing bytes"));
    }
    Ok(ids)
}

/// `Identifier.parse` and `toString`: the full `namespace:path`, `None` for
/// characters an identifier can't hold.
pub fn normalize(s: &str) -> Option<String> {
    let (ns, path) = s.split_once(':').unwrap_or(("minecraft", s));
    let ns = if ns.is_empty() { "minecraft" } else { ns };
    let ok_ns = ns
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b));
    let ok_path = path
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-./".contains(&b));
    (ok_ns && ok_path).then(|| format!("{ns}:{path}"))
}

/// A synced attachment type a native mod reads, as the mod registered it on
/// its Fabric side (`AttachmentRegistry.Builder.syncWith`, `copyOnDeath`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub id: String,
    /// `maxSyncSize`, raised to [`DEFAULT_MAX_DATA_SIZE`] as Fabric raises it.
    pub max_size: usize,
    /// `copyOnDeath`: the value stays on the player through a death respawn.
    pub copy_on_death: bool,
}

static ACCEPTED: RwLock<Vec<Accepted>> = RwLock::new(Vec::new());

/// Names a synced Fabric attachment type for the next Fabric server Horizon
/// joins (`AttachmentRegistryImpl.register` of a synced type). A second call
/// for the same id replaces the first. An error for an id that isn't one, or
/// is longer than [`MAX_IDENTIFIER_SIZE`], as Fabric refuses to register it.
pub fn accept(id: &str, max_size: usize, copy_on_death: bool) -> Result<()> {
    let id = normalize(id).ok_or_else(|| Error::new(format!("bad attachment identifier {id}")))?;
    if id.chars().count() > MAX_IDENTIFIER_SIZE {
        return Err(Error::new(format!(
            "Identifier length is too long for a synced attachment type (was {}, maximum is {MAX_IDENTIFIER_SIZE})",
            id.chars().count()
        )));
    }
    let entry = Accepted {
        max_size: max_size.max(DEFAULT_MAX_DATA_SIZE),
        id,
        copy_on_death,
    };
    let mut accepted = ACCEPTED.write().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = accepted.iter_mut().find(|a| a.id == entry.id) {
        *existing = entry;
    } else if accepted.len() >= MAX_ACCEPTED {
        return Err(Error::new("too many accepted attachments"));
    } else {
        accepted.push(entry);
    }
    Ok(())
}

/// What [`accept`] named so far.
pub fn accepted() -> Vec<Accepted> {
    ACCEPTED.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The answer to the server's request: the ids of [`accepted`]
/// (`AttachmentSync.createResponsePayload`).
pub fn accepted_payload(accepted: &[Accepted]) -> Vec<u8> {
    let ids: Vec<&str> = accepted.iter().map(|a| a.id.as_str()).collect();
    encode_accepted(&ids).unwrap_or_else(|_| vec![0])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes the identifier as `Utf8String.write`: VarInt byte length, UTF-8.
    fn utf(s: &str) -> Vec<u8> {
        let mut out = vec![s.len() as u8];
        out.extend_from_slice(s.as_bytes());
        out
    }

    fn only(id: &'static str, max: usize) -> impl Fn(&str) -> Option<usize> {
        move |got| (got == id).then_some(max)
    }

    #[test]
    fn the_constants_are_fabrics() {
        // 1048576 - (9 + 256) - (1 + 75 + 10), AttachmentSync's static block.
        assert_eq!(DEFAULT_MAX_DATA_SIZE, 1_048_225);
        assert_eq!(DEFAULT_SYNC_PACKET_SIZE, 1_048_490);
    }

    /// Each target's bytes, written by hand from `AttachmentTargetInfo`'s
    /// codecs: the type byte, then BlockPos.asLong big-endian, a VarInt, a
    /// VarLong of `ChunkPos.pack`, or nothing.
    #[test]
    fn every_target_has_fabrics_bytes() {
        let cases: Vec<(Target, Vec<u8>)> = vec![
            // (1, 2, 3): 1 << 38 | 3 << 12 | 2.
            (
                Target::BlockEntity([1, 2, 3]),
                vec![0, 0, 0, 0, 0x40, 0, 0, 0x30, 0x02],
            ),
            // (-1, -64, -1): every x and z bit, y = 0xFC0.
            (
                Target::BlockEntity([-1, -64, -1]),
                vec![0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xC0],
            ),
            (Target::Entity(300), vec![1, 0xAC, 0x02]),
            (Target::Entity(-1), vec![1, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F]),
            // pack(1, 2) = 0x2_0000_0001.
            (Target::Chunk([1, 2]), vec![2, 0x81, 0x80, 0x80, 0x80, 0x20]),
            // pack(-1, 0) = 0xFFFF_FFFF.
            (
                Target::Chunk([-1, 0]),
                vec![2, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F],
            ),
            // pack(0, -1) = 0xFFFF_FFFF_0000_0000: a ten-byte VarLong.
            (
                Target::Chunk([0, -1]),
                vec![
                    2, 0x80, 0x80, 0x80, 0x80, 0xF0, 0xFF, 0xFF, 0xFF, 0xFF, 0x01,
                ],
            ),
            (Target::World, vec![3]),
            (Target::Global, vec![4]),
        ];
        for (target, bytes) in cases {
            let mut w = Writer::new();
            encode_target(&mut w, target);
            assert_eq!(w.data, bytes, "{target:?}");
            let mut r = Reader::new(&bytes);
            assert_eq!(decode_target(&mut r).unwrap(), target);
            assert_eq!(r.remaining(), 0);
        }
    }

    #[test]
    fn a_change_is_target_id_flag_and_the_rest() {
        // An entity's value: an int written by ByteBufCodecs.INT (4 bytes).
        let mut set = vec![1, 0x05];
        set.extend(utf("mymod:mana"));
        set.extend([1, 0, 0, 0, 42]);
        let got = decode_change(&set, only("mymod:mana", DEFAULT_MAX_DATA_SIZE)).unwrap();
        assert_eq!(
            got,
            Change {
                target: Target::Entity(5),
                id: "mymod:mana".into(),
                value: Some(vec![0, 0, 0, 42]),
            }
        );
        assert_eq!(encode_change(&got, DEFAULT_MAX_DATA_SIZE).unwrap(), set);

        // A removal on the level: no value bytes.
        let mut removed = vec![3];
        removed.extend(utf("mymod:mana"));
        removed.push(0);
        let got = decode_change(&removed, only("mymod:mana", DEFAULT_MAX_DATA_SIZE)).unwrap();
        assert_eq!((got.target, got.value), (Target::World, None));

        // An empty value is a value (a unit codec writes nothing).
        let mut unit = vec![4];
        unit.extend(utf("mymod:flag"));
        unit.push(1);
        let got = decode_change(&unit, only("mymod:flag", DEFAULT_MAX_DATA_SIZE)).unwrap();
        assert_eq!(got.value, Some(vec![]));
    }

    #[test]
    fn sizes_unknown_types_and_cut_payloads_are_errors() {
        let mut head = vec![2, 0x81, 0x80, 0x80, 0x80, 0x20];
        head.extend(utf("mymod:big"));
        head.push(1);
        let mut at_max = head.clone();
        at_max.extend([7; 16]);
        assert!(decode_change(&at_max, only("mymod:big", 16)).is_ok());
        let mut over = at_max.clone();
        over.push(7);
        assert!(decode_change(&over, only("mymod:big", 16)).is_err());
        let too_big = Change {
            target: Target::World,
            id: "mymod:big".into(),
            value: Some(vec![0; 17]),
        };
        assert!(encode_change(&too_big, 16).is_err());
        // A type the client didn't accept.
        assert!(decode_change(&at_max, only("mymod:other", 16)).is_err());
        // Every prefix of a change short of its flag fails, never panics.
        for len in 0..head.len() {
            assert!(
                decode_change(&head[..len], only("mymod:big", 16)).is_err(),
                "{len}"
            );
        }
        // Unknown target type, a bad identifier.
        assert!(decode_change(&[5, 0], only("mymod:big", 16)).is_err());
        let mut bad = vec![3];
        bad.extend(utf("Bad Id"));
        bad.push(0);
        assert!(decode_change(&bad, |_| Some(16)).is_err());
        // An identifier length past what is left.
        assert!(decode_change(&[3, 0xFF, 0xFF, 0x01, b'a'], |_| Some(16)).is_err());
    }

    #[test]
    fn garbage_never_panics() {
        let mut seed = 0x1234_5678u32;
        for len in 0..256 {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (seed >> 16) as u8
                })
                .collect();
            let _ = decode_change(&data, |_| Some(8));
            let _ = decode_accepted(&data);
        }
    }

    #[test]
    fn the_answer_is_a_counted_set_of_identifiers() {
        let mut expected = vec![2];
        expected.extend(utf("mymod:mana"));
        expected.extend(utf("other:x"));
        assert_eq!(
            encode_accepted(&["mymod:mana", "other:x"]).unwrap(),
            expected
        );
        assert_eq!(
            decode_accepted(&expected).unwrap(),
            ["mymod:mana", "other:x"]
        );
        // No accepted types: an empty set.
        assert_eq!(accepted_payload(&[]), [0]);
        // A bare path is Identifier.parse's minecraft namespace.
        assert_eq!(decode_accepted(&[1, 1, b'a']).unwrap(), ["minecraft:a"]);
        // 8193 elements, a cut list, trailing bytes.
        assert!(decode_accepted(&[0x81, 0x40]).is_err());
        assert!(decode_accepted(&expected[..expected.len() - 1]).is_err());
        let mut padded = expected.clone();
        padded.push(0);
        assert!(decode_accepted(&padded).is_err());
        let many: Vec<&str> = vec!["a:b"; MAX_ACCEPTED + 1];
        assert!(encode_accepted(&many).is_err());
    }

    #[test]
    fn accept_raises_small_sizes_and_refuses_long_ids() {
        accept("attachtest:small", 10, true).unwrap();
        accept("attachtest:large", 5_000_000, false).unwrap();
        let all = accepted();
        let small = all.iter().find(|a| a.id == "attachtest:small").unwrap();
        assert_eq!(
            (small.max_size, small.copy_on_death),
            (DEFAULT_MAX_DATA_SIZE, true)
        );
        let large = all.iter().find(|a| a.id == "attachtest:large").unwrap();
        assert_eq!(large.max_size, 5_000_000);
        let long = format!("attachtest:{}", "a".repeat(MAX_IDENTIFIER_SIZE));
        assert!(accept(&long, 0, false).is_err());
        assert!(accept("Attach Test", 0, false).is_err());
    }
}
