//! Fabric API networking-api-v1: the packet splitter for oversized custom
//! payloads (`impl/networking/splitter/`: `FabricSplitPacketPayload`,
//! `FabricPacketSplitter`, `FabricPacketMerger`; the caps of
//! `PayloadTypeRegistryImpl`).
//!
//! A payload type a mod registers with `registerLarge` may be larger than the
//! vanilla cap of one custom payload packet. Fabric encodes the whole packet
//! (packet id, payload id, payload) and, when that is too big for one packet,
//! sends it as a run of `fabric:split` custom payloads: the first starts with
//! the VarInt length of the whole encoded packet, the others carry the rest in
//! order. The receiver glues them back together and decodes the result as the
//! packet it was. Nothing is negotiated: every Fabric connection has the
//! merger in configuration and play, and only payload types the receiver
//! knows as large may arrive split, up to their cap.
//!
//! Horizon's [`Merger`] works on raw packets (id and body, as the connection
//! reads them), so it serves configuration and play alike. Where Fabric
//! disconnects on a broken run, Horizon drops the run and keeps playing: a
//! server can't panic or stall the client with split data.
//!
//! [`channels`] keeps the play channels each side registered
//! (`minecraft:register`, `minecraft:unregister`, `c:register`).
use super::REGISTRY_SYNC;

pub mod channels;

use crate::{
    Error, Result,
    buf::{Reader, Writer, varint_size, write_varint},
};

/// `FabricSplitPacketPayload.TYPE`.
pub const SPLIT: &str = "fabric:split";
/// `ClientboundCustomPayloadPacket.MAX_PAYLOAD_SIZE`, Fabric's
/// `SAFE_S2C_SPLIT_SIZE`: the largest part a server sends.
pub const SAFE_S2C_SPLIT_SIZE: usize = 1_048_576;
/// `ServerboundCustomPayloadPacket.MAX_PAYLOAD_SIZE`, Fabric's
/// `SAFE_C2S_SPLIT_SIZE`: the largest part a client sends.
pub const SAFE_C2S_SPLIT_SIZE: usize = 32_767;
/// `ClientboundRecipeSyncPayload.TYPE`.
pub const RECIPE_SYNC: &str = "fabric:recipe_sync";
/// `RecipeSyncImpl.RECIPE_PAYLOAD_MAX_SIZE`.
pub const RECIPE_SYNC_MAX: usize = 64 * 1024 * 1024;
/// `FabricRegistryInit.MAX_PACKET_SIZE` (the default of
/// `fabric.registry.sync.max_packet_size`).
pub const REGISTRY_SYNC_MAX: usize = 128 * 1024 * 1024;

/// The payload types that may be split, each with its padded cap: one
/// direction and phase of `PayloadTypeRegistryImpl.maxPacketSizes`.
#[derive(Clone, Debug, Default)]
pub struct Limits {
    caps: Vec<(String, usize)>,
    /// `minimalSplittableSize`: the part size of this direction.
    part: usize,
}

impl Limits {
    /// No large payload types, receiving from a server.
    pub fn clientbound() -> Self {
        Self {
            caps: Vec::new(),
            part: SAFE_S2C_SPLIT_SIZE,
        }
    }
    /// No large payload types, sending to a server.
    pub fn serverbound() -> Self {
        Self {
            caps: Vec::new(),
            part: SAFE_C2S_SPLIT_SIZE,
        }
    }
    /// What Fabric API registers large for a client in configuration: the
    /// registry sync.
    pub fn fabric_clientbound_configuration() -> Self {
        Self::clientbound().allow(REGISTRY_SYNC, REGISTRY_SYNC_MAX)
    }
    /// What Fabric API registers large for a client in play with a fixed
    /// cap: the recipe sync. (`fabric:attachment_sync_v1` gets a cap each
    /// side sums from its mods' attachment types; its module adds it.)
    pub fn fabric_clientbound_play() -> Self {
        Self::clientbound().allow(RECIPE_SYNC, RECIPE_SYNC_MAX)
    }
    /// `registerLarge(type, codec, max)`: `max` is the payload's own limit,
    /// padded as `padAndSetMaxPacketSize` pads it. A cap no larger than one
    /// part needs no splitting and is not kept, as in Fabric.
    pub fn allow(mut self, channel: &str, max: usize) -> Self {
        self.caps.retain(|(c, _)| c != channel);
        let cap = padded(channel, max);
        if cap > self.part {
            self.caps.push((channel.to_owned(), cap));
        }
        self
    }
    /// `getMaxPacketSizeForSplitting`: the padded cap, `None` (Fabric's -1)
    /// for a payload type that is never split.
    pub fn cap(&self, channel: &str) -> Option<usize> {
        self.caps
            .iter()
            .find(|(c, _)| c == channel)
            .map(|&(_, cap)| cap)
    }
}

/// `padAndSetMaxPacketSize`: the payload's cap plus room for the packet id
/// and the payload id (`ByteBufUtil.utf8MaxBytes`: 3 bytes a char), at most
/// `Integer.MAX_VALUE`.
fn padded(channel: &str, max: usize) -> usize {
    let identifier = channel.encode_utf16().count() * 3;
    let padding = varint_size(identifier as i32) + identifier + 5 * 2;
    max.saturating_add(padding).min(i32::MAX as usize)
}

/// A run of split packets on its way.
#[derive(Debug)]
enum State {
    Idle,
    /// `FabricPacketMerger.Merger`: the packet so far and its final size.
    Merging {
        channel: String,
        size: usize,
        data: Vec<u8>,
    },
    /// A run Horizon refused: its remaining bytes are swallowed.
    Skipping {
        left: usize,
    },
}

/// What [`Merger::feed`] made of a packet.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// A packet to decode as usual: an ordinary one, or the packet a run's
    /// last part completed (its id and body, as the connection reads them).
    Packet { id: i32, body: Vec<u8> },
    /// Part of a run, held until the rest arrives.
    Held,
    /// A broken or refused run: nothing to decode. Fabric disconnects here.
    Dropped(Error),
}

/// `FabricPacketMerger` for one connection in one phase.
#[derive(Debug)]
pub struct Merger {
    /// The phase's clientbound custom payload packet id.
    custom_payload: i32,
    limits: Limits,
    state: State,
}

impl Merger {
    /// A merger for the phase whose custom payload packet id is
    /// `custom_payload`, taking the payload types of `limits` split.
    pub fn new(custom_payload: i32, limits: Limits) -> Self {
        Self {
            custom_payload,
            limits,
            state: State::Idle,
        }
    }

    /// Whether a run is under way.
    pub fn merging(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// `FabricPacketMerger.decode` for the packet the connection just read:
    /// its id, then the body after it.
    pub fn feed(&mut self, id: i32, body: Vec<u8>) -> Step {
        let split = if id == self.custom_payload {
            split_part(&body)
        } else {
            None
        };
        let Some(at) = split else {
            if let State::Merging { channel, .. } = &self.state {
                // Fabric refuses any other packet inside a run; Horizon
                // drops the run and keeps the packet.
                log::debug!("Fabric: split packet '{channel}' interrupted by packet {id}");
            }
            self.state = State::Idle;
            return Step::Packet { id, body };
        };
        let part = &body[at..];
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Idle => self.start(part),
            State::Skipping { left } => {
                let left = left.saturating_sub(part.len());
                if left > 0 {
                    self.state = State::Skipping { left };
                }
                Step::Held
            }
            State::Merging {
                channel,
                size,
                mut data,
            } => {
                // Merger.add.
                let total = data.len() + part.len();
                if total > size {
                    return Step::Dropped(Error::new(format!(
                        "Received too much data for packet '{channel}'! Expected {size} bytes, received {total} bytes!"
                    )));
                }
                data.extend_from_slice(part);
                if data.len() < size {
                    self.state = State::Merging {
                        channel,
                        size,
                        data,
                    };
                    return Step::Held;
                }
                let mut r = Reader::new(&data);
                match r.varint() {
                    Ok(id) => {
                        let start = r.position();
                        data.drain(..start);
                        Step::Packet { id, body: data }
                    }
                    Err(e) => Step::Dropped(e),
                }
            }
        }
    }

    /// A run's first part: the whole packet's size, then the packet starts.
    /// Its id must be the custom payload's; its payload id says whose cap
    /// applies.
    fn start(&mut self, part: &[u8]) -> Step {
        let mut r = Reader::new(part);
        let size = match r.varint() {
            Ok(size) => size,
            Err(e) => return Step::Dropped(e),
        };
        let packet = r.rest();
        let Ok(size) = usize::try_from(size) else {
            return Step::Dropped(Error::new(format!("split packet of size {size}")));
        };
        let mut head = Reader::new(packet);
        match head.varint() {
            Ok(id) if id == self.custom_payload => {}
            Ok(id) => {
                return Step::Dropped(Error::new(format!(
                    "Received unsupported split packet type! Expected {} got {id}",
                    self.custom_payload
                )));
            }
            Err(e) => return Step::Dropped(e),
        }
        let channel = match head.string(32767) {
            Ok(c) => c,
            Err(e) => return Step::Dropped(e),
        };
        let refused = match self.limits.cap(&channel) {
            None => Some(format!(
                "Received '{channel}' packet doesn't support splitting, but received split data!"
            )),
            Some(cap) if cap < size => Some(format!(
                "Received '{channel}' packet is larger than max allowed size! Got {size} bytes, expected {cap} bytes!"
            )),
            Some(_) => None,
        };
        if let Some(message) = refused {
            let left = size.saturating_sub(packet.len());
            if left > 0 {
                self.state = State::Skipping { left };
            }
            return Step::Dropped(Error::new(message));
        }
        if packet.len() >= size {
            return Step::Dropped(Error::new(format!(
                "Received '{channel}' as a split packet, but it wasn't actually split!"
            )));
        }
        // Fabric allocates the final size at once; Horizon grows the buffer
        // as the data arrives, so a lying header costs nothing.
        let mut data = Vec::with_capacity(size.min(2 * SAFE_S2C_SPLIT_SIZE));
        data.extend_from_slice(packet);
        self.state = State::Merging {
            channel,
            size,
            data,
        };
        Step::Held
    }

    /// `clearMerger`: the connection changes phase or closes.
    pub fn clear(&mut self) {
        self.state = State::Idle;
    }
}

/// Where the part starts in a `fabric:split` custom payload body; `None`
/// for any other payload.
fn split_part(body: &[u8]) -> Option<usize> {
    let mut r = Reader::new(body);
    (r.string(32767).ok()? == SPLIT).then(|| r.position())
}

/// `ServerboundCustomPayloadPacketMixin.fabric_split` with
/// `FabricPacketSplitter.genericPacketSplitter`: the packet bodies (id, then
/// payload) that carry `data` on `channel` to a server whose Fabric mod
/// registered `channel` large (`limits`). `custom_payload` is the phase's
/// serverbound custom payload id. A payload type `limits` doesn't know goes
/// out whole; one beyond its cap is an error (Fabric's `EncoderException`).
pub fn split(
    custom_payload: i32,
    channel: &str,
    data: &[u8],
    limits: &Limits,
) -> Result<Vec<Vec<u8>>> {
    let mut w = Writer::new();
    w.varint(custom_payload);
    w.string(channel);
    w.bytes(data);
    let packet = w.data;
    let Some(cap) = limits.cap(channel) else {
        return Ok(vec![packet]);
    };
    let part = limits.part;
    if packet.len() < part {
        // PassthroughPacket.
        return Ok(vec![packet]);
    }
    if packet.len() > cap {
        return Err(Error::new(format!(
            "Packet '{channel}' may not be larger than {cap} bytes!"
        )));
    }
    let wrap = |bytes: &[u8]| {
        let mut w = Writer::new();
        w.varint(custom_payload);
        w.string(SPLIT);
        w.bytes(bytes);
        w.data
    };
    // The first part starts with the whole size and is shorter by it.
    let mut first = Vec::with_capacity(part);
    write_varint(&mut first, packet.len() as i32);
    let head = part - first.len();
    first.extend_from_slice(&packet[..head]);
    let mut out = vec![wrap(&first)];
    out.extend(packet[head..].chunks(part).map(wrap));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The play custom payload ids of 26.3: clientbound 24, serverbound 22.
    const CB: i32 = crate::ids::play::clientbound::CUSTOM_PAYLOAD;
    const SB: i32 = crate::ids::play::serverbound::CUSTOM_PAYLOAD;

    /// A `fabric:split` custom payload body by hand: `Identifier.STREAM_CODEC`
    /// (VarInt length 12, the ASCII), then `FabricSplitPacketPayload.write`
    /// (the raw bytes).
    fn split_body(part: &[u8]) -> Vec<u8> {
        let mut b = vec![12];
        b.extend_from_slice(b"fabric:split");
        b.extend_from_slice(part);
        b
    }

    /// What `PacketEncoder` writes for a custom payload on
    /// `fabric:recipe_sync`: VarInt id, VarInt 18 and the ASCII, the data.
    fn recipe_packet(id: u8, data: &[u8]) -> Vec<u8> {
        let mut p = vec![id, 18];
        p.extend_from_slice(b"fabric:recipe_sync");
        p.extend_from_slice(data);
        p
    }

    fn varint(v: usize) -> Vec<u8> {
        let mut out = Vec::new();
        write_varint(&mut out, v as i32);
        out
    }

    #[test]
    fn caps_are_padded_as_pad_and_set_max_packet_size_pads_them() {
        // "fabric:recipe_sync": 18 chars, 54 max UTF-8 bytes, VarInt of 54 is
        // 1 byte, plus 5 * 2: 65 bytes over the payload's own cap.
        let limits = Limits::fabric_clientbound_play();
        assert_eq!(limits.cap(RECIPE_SYNC), Some(64 * 1024 * 1024 + 65));
        // "fabric:registry/sync": 20 chars: 60 + 1 + 10.
        let limits = Limits::fabric_clientbound_configuration();
        assert_eq!(limits.cap(REGISTRY_SYNC), Some(128 * 1024 * 1024 + 71));
        // A cap within one part is no large payload type.
        let small = Limits::clientbound().allow("a:b", 1000);
        assert_eq!(small.cap("a:b"), None);
        assert_eq!(small.cap("c:d"), None);
        // Overflow stops at Integer.MAX_VALUE.
        let huge = Limits::clientbound().allow("a:b", usize::MAX);
        assert_eq!(huge.cap("a:b"), Some(i32::MAX as usize));
    }

    /// A server's run, split by hand as `genericPacketSplitter` splits with
    /// parts of 1048576: a 1048600-byte packet goes as VarInt 1048600
    /// (3 bytes) and 1048573 packet bytes, then the last 27.
    #[test]
    fn a_split_run_is_glued_back_into_its_packet() {
        let data: Vec<u8> = (0..1_048_600 - 20).map(|i| (i % 251) as u8).collect();
        let packet = recipe_packet(CB as u8, &data);
        assert_eq!(packet.len(), 1_048_600);
        let mut first = vec![0x98, 0x80, 0x40];
        assert_eq!(first, varint(1_048_600));
        first.extend_from_slice(&packet[..1_048_573]);
        assert_eq!(first.len(), SAFE_S2C_SPLIT_SIZE);
        let second = &packet[1_048_573..];
        assert_eq!(second.len(), 27);

        let mut m = Merger::new(CB, Limits::fabric_clientbound_play());
        assert_eq!(m.feed(CB, split_body(&first)), Step::Held);
        assert!(m.merging());
        let Step::Packet { id, body } = m.feed(CB, split_body(second)) else {
            panic!("not glued");
        };
        assert!(!m.merging());
        assert_eq!(id, CB);
        assert_eq!(body, packet[1..]);
        // The glued body decodes as an ordinary custom payload.
        let crate::packets::play::Clientbound::CustomPayload(p) =
            crate::packets::play::Clientbound::decode(id, &body).unwrap()
        else {
            panic!("not a custom payload");
        };
        assert_eq!(p.channel, RECIPE_SYNC);
        assert_eq!(p.data, data);
    }

    #[test]
    fn ordinary_packets_pass_through() {
        let mut m = Merger::new(CB, Limits::fabric_clientbound_play());
        let mut brand = vec![15];
        brand.extend_from_slice(b"minecraft:brand");
        brand.extend_from_slice(&[6, b'f', b'a', b'b', b'r', b'i', b'c']);
        assert_eq!(
            m.feed(CB, brand.clone()),
            Step::Packet {
                id: CB,
                body: brand
            }
        );
        // A split-looking body under another packet id is no split payload.
        let body = split_body(&[1, 2, 3]);
        assert_eq!(m.feed(3, body.clone()), Step::Packet { id: 3, body });
        // Nor is an empty custom payload body.
        assert!(matches!(m.feed(CB, vec![]), Step::Packet { .. }));
    }

    #[test]
    fn refused_runs_are_skipped_whole() {
        // A run for a channel Horizon never takes split: its header says 40
        // bytes, 20 come now and 20 later; both parts go, the next packet
        // stays.
        let mut m = Merger::new(CB, Limits::fabric_clientbound_play());
        let mut packet = vec![CB as u8, 7];
        packet.extend_from_slice(b"mod:big");
        packet.resize(40, 9);
        let mut first = vec![40];
        first.extend_from_slice(&packet[..20]);
        assert!(matches!(
            m.feed(CB, split_body(&first)),
            Step::Dropped(e) if e.0.contains("doesn't support splitting")
        ));
        assert_eq!(m.feed(CB, split_body(&packet[20..])), Step::Held);
        assert!(!m.merging());
        assert!(matches!(m.feed(5, vec![1]), Step::Packet { id: 5, .. }));

        // Over the cap: a header of 64 MiB + 66.
        let mut first = varint(64 * 1024 * 1024 + 66);
        first.extend_from_slice(&recipe_packet(CB as u8, &[0; 100]));
        assert!(matches!(
            m.feed(CB, split_body(&first)),
            Step::Dropped(e) if e.0.contains("larger than max allowed size")
        ));
        assert!(m.merging());
        // At the cap is fine.
        let mut m = Merger::new(CB, Limits::fabric_clientbound_play());
        let mut first = varint(64 * 1024 * 1024 + 65);
        first.extend_from_slice(&recipe_packet(CB as u8, &[0; 100]));
        assert_eq!(m.feed(CB, split_body(&first)), Step::Held);
    }

    #[test]
    fn broken_runs_never_panic() {
        let limits = Limits::fabric_clientbound_play();
        let mut whole = varint(29);
        whole.extend_from_slice(&recipe_packet(CB as u8, &[0; 9]));
        let cases: Vec<Vec<u8>> = vec![
            vec![],                             // no size
            vec![0xFF, 0xFF, 0xFF],             // truncated VarInt
            vec![0x80, 0x80, 0x80, 0x80, 0x0F], // negative size
            vec![10],                           // a size and no packet
            vec![10, 0x80],                     // truncated packet id
            vec![10, 3, 1, 2],                  // another packet's id
            vec![10, CB as u8, 200],            // truncated payload id
            vec![10, CB as u8, 2, 0xFF, 0xFE],  // payload id not UTF-8
            whole,                              // the whole packet in one part
        ];
        for case in cases {
            let mut m = Merger::new(CB, limits.clone());
            assert!(
                matches!(m.feed(CB, split_body(&case)), Step::Dropped(_)),
                "{case:?}"
            );
            assert!(!m.merging(), "{case:?}");
        }
        // Too much data in a later part ends the run.
        let mut m = Merger::new(CB, limits.clone());
        let mut first = varint(50);
        first.extend_from_slice(&recipe_packet(CB as u8, &[0; 9]));
        assert_eq!(m.feed(CB, split_body(&first)), Step::Held);
        assert!(matches!(
            m.feed(CB, split_body(&[0; 40])),
            Step::Dropped(e) if e.0.contains("too much data")
        ));
        assert!(!m.merging());
        // Another packet inside a run: the run goes, the packet stays.
        let mut m = Merger::new(CB, limits);
        assert_eq!(m.feed(CB, split_body(&first)), Step::Held);
        assert_eq!(
            m.feed(7, vec![1, 2]),
            Step::Packet {
                id: 7,
                body: vec![1, 2]
            }
        );
        assert!(!m.merging());
    }

    #[test]
    fn a_lying_header_allocates_nothing_up_front() {
        let cfg = crate::ids::configuration::clientbound::CUSTOM_PAYLOAD;
        let mut m = Merger::new(cfg, Limits::fabric_clientbound_configuration());
        let mut packet = vec![cfg as u8, 20];
        packet.extend_from_slice(b"fabric:registry/sync");
        let mut first = varint(128 * 1024 * 1024);
        first.extend_from_slice(&packet);
        assert_eq!(m.feed(cfg, split_body(&first)), Step::Held);
        let State::Merging { data, .. } = &m.state else {
            panic!("not merging");
        };
        assert!(data.capacity() < 128 * 1024 * 1024);
        m.clear();
        assert!(!m.merging());
    }

    /// `genericPacketSplitter` with serverbound parts of 32767: a packet of
    /// 40000 bytes goes as VarInt 40000 (3 bytes) and 32764 bytes, then the
    /// last 7236, each in a `fabric:split` custom payload.
    #[test]
    fn a_large_serverbound_payload_is_split_as_fabric_splits_it() {
        let limits = Limits::serverbound().allow("mod:big", 100_000);
        let mut packet = vec![SB as u8, 7];
        packet.extend_from_slice(b"mod:big");
        let data: Vec<u8> = (0..40_000 - 9).map(|i| (i % 7) as u8).collect();
        packet.extend_from_slice(&data);
        assert_eq!(packet.len(), 40_000);

        let parts = split(SB, "mod:big", &data, &limits).unwrap();
        assert_eq!(parts.len(), 2);
        let mut first = vec![SB as u8];
        first.extend_from_slice(&split_body(&[0xC0, 0xB8, 0x02]));
        first.extend_from_slice(&packet[..32_764]);
        assert_eq!(parts[0], first);
        let mut second = vec![SB as u8];
        second.extend_from_slice(&split_body(&packet[32_764..]));
        assert_eq!(parts[1], second);
        // Each part's payload fits the vanilla serverbound cap exactly.
        assert_eq!(parts[0].len() - 1 - 13, SAFE_C2S_SPLIT_SIZE);
    }

    #[test]
    fn small_unknown_and_oversized_serverbound_payloads() {
        let limits = Limits::serverbound().allow("mod:big", 100_000);
        let zeros = vec![0u8; 200_000];
        // Below one part: the packet as it is (PassthroughPacket).
        let mut whole = vec![SB as u8, 7];
        whole.extend_from_slice(b"mod:big");
        whole.extend_from_slice(&[1, 2, 3]);
        assert_eq!(split(SB, "mod:big", &[1, 2, 3], &limits).unwrap(), [whole]);
        // Exactly one part long is split already (`<`, not `<=`).
        let at = SAFE_C2S_SPLIT_SIZE - 9;
        assert_eq!(
            split(SB, "mod:big", &zeros[..at], &limits).unwrap().len(),
            2
        );
        assert_eq!(
            split(SB, "mod:big", &zeros[..at - 1], &limits)
                .unwrap()
                .len(),
            1
        );
        // Not registered large: whole, whatever its size.
        assert_eq!(
            split(SB, "a:b", &zeros[..50_000], &limits).unwrap().len(),
            1
        );
        // The padded cap: 100000 + 7 * 3 + 1 + 10 = 100032 bytes of packet.
        assert!(split(SB, "mod:big", &zeros[..100_032 - 9], &limits).is_ok());
        assert!(split(SB, "mod:big", &zeros[..100_032 - 8], &limits).is_err());
    }

    /// The splitter and the merger agree across part boundaries.
    #[test]
    fn what_the_splitter_cuts_the_merger_glues() {
        for len in [32_767 - 9, 65_534, 65_535 + 3, 98_000] {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 % 256) as u8).collect();
            let limits = Limits::serverbound().allow("mod:big", 100_000);
            let parts = split(SB, "mod:big", &data, &limits).unwrap();
            let mut m = Merger::new(SB, limits);
            let mut got = None;
            for p in parts {
                let mut r = Reader::new(&p);
                let id = r.varint().unwrap();
                if let Step::Packet { id, body } = m.feed(id, r.rest().to_vec()) {
                    got = Some((id, body));
                }
            }
            let (id, body) = got.unwrap();
            assert_eq!(id, SB);
            let mut r = Reader::new(&body);
            assert_eq!(r.string(32767).unwrap(), "mod:big");
            assert_eq!(r.rest(), data);
        }
    }
}
