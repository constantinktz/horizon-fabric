//! Fabric API particles-v1: extended block particle options
//! (`ExtendedBlockParticleOptionSync`, `ExtendedBlockParticleOptionStreamCodec`,
//! `ExtendedBlockParticleOptionSyncClient`, Fabric API 0.162.0+26.3).
//!
//! A Fabric server can give a `BlockParticleOption` (`block`, `block_marker`,
//! `falling_dust`, `dust_pillar`, `block_crumble`) the position its block
//! state was read from (`FabricBlockParticleOption.create`: fall damage,
//! running, brushing, the warden's digging ...). The client uses it for the
//! terrain particle's tint (`TerrainParticleMixin`), where vanilla takes the
//! block the particle spawns in.
//!
//! The position only crosses the wire when the client registered
//! [`CHANNEL`] as a configuration channel it receives: the server checks
//! `ServerConfigurationNetworking.canSend` once in `CONFIGURE` and keeps the
//! answer in the connection's packet context, so it holds for the play
//! phase too. Then an option with a position is written as
//! `VarInt(-1)`, the vanilla option (the block state id as a VarInt), then
//! `BlockPos.STREAM_CODEC` (one packed long); an option without a position
//! stays vanilla. The decoder takes both forms.
//!
//! The flag a connection carries is thread local, as `adapter::ids`'
//! renumbering is: the network thread that decodes a connection's packets
//! opens a [`Scope`] for it and calls [`negotiate`] once it has registered
//! the channel; `entity_data::Particle` reads [`negotiated`].
use crate::{
    Error, Result,
    buf::{Codec, Pos, Reader, Writer},
};
use std::cell::Cell;

/// `ExtendedBlockParticleOptionSync.PACKET_ID`: the configuration channel a
/// client registers to receive block positions. The server never sends a
/// payload on it (`DummyPayload` only exists so the client can register a
/// receiver).
pub const CHANNEL: &str = "fabric:extended_block_particle_option_sync";
/// `ExtendedBlockParticleOptionStreamCodec.PACKET_MARKER`. Vanilla block
/// state ids are never negative, so it can't be mistaken for one.
pub const MARKER: i32 = -1;

thread_local! {
    static NEGOTIATED: Cell<bool> = const { Cell::new(false) };
}

/// Whether the connection this thread decodes registered [`CHANNEL`].
pub fn negotiated() -> bool {
    NEGOTIATED.get()
}

/// This client registered [`CHANNEL`] with the server (it sent it in its
/// `minecraft:register`): the server may now send block positions.
pub fn negotiate() {
    NEGOTIATED.set(true);
}

/// One connection's lifetime on this thread: starts un-negotiated and puts
/// back what was there before when dropped.
#[must_use = "the flag resets when the scope drops"]
pub struct Scope(bool);

impl Scope {
    pub fn new() -> Scope {
        Scope(NEGOTIATED.replace(false))
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        NEGOTIATED.set(self.0);
    }
}

/// A `BlockParticleOption`'s wire data: the block state id (as the server
/// numbers it) and Fabric's optional block position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockOption {
    pub state: i32,
    pub pos: Option<[i32; 3]>,
}

/// `ExtendedBlockParticleOptionStreamCodec.decode` when `extended`, else
/// vanilla's `BlockParticleOption.streamCodec` (a VarInt state id).
pub fn read_block(r: &mut Reader, extended: bool) -> Result<BlockOption> {
    let first = r.varint()?;
    if !extended || first != MARKER {
        // The fallback codec is exactly the VarInt just read.
        return Ok(BlockOption {
            state: first,
            pos: None,
        });
    }
    let state = r.varint()?;
    if state < 0 {
        // `IdMapper.byId` has no entry for it: no block state to draw.
        return Err(Error::new(format!(
            "extended block particle option: block state id {state}"
        )));
    }
    let pos = Pos::read(r)?;
    Ok(BlockOption {
        state,
        pos: Some(pos),
    })
}

/// `ExtendedBlockParticleOptionStreamCodec.encode`: the position goes out
/// only when there is one and the other side registered [`CHANNEL`]
/// (`shouldEncodeFallback` is false).
pub fn write_block(w: &mut Writer, option: BlockOption, extended: bool) {
    match option.pos {
        Some(pos) if extended => {
            w.varint(MARKER);
            w.varint(option.state);
            Pos::write(&pos, w);
        }
        _ => w.varint(option.state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Expected bytes from a Java run against the 26.3 server jar:
    /// `writeVarInt(-1)`, `ByteBufCodecs.VAR_INT.encode(state)`,
    /// `BlockPos.STREAM_CODEC.encode(new BlockPos(x, y, z))`, the steps of
    /// `ExtendedBlockParticleOptionStreamCodec.encode`.
    const JAVA: [(i32, [i32; 3], &str); 4] = [
        (1, [0, 0, 0], "ffffffff0f010000000000000000"),
        (9, [123, -60, -456], "ffffffff0f0900001effffe38fc4"),
        (
            27000,
            [-30000000, -2048, 29999999],
            "ffffffff0ff8d2018d8f201c9c37f800",
        ),
        (300, [1, 2047, -1], "ffffffff0fac020000007ffffff7ff"),
    ];

    #[test]
    fn decodes_the_extended_form_fabric_writes() {
        for (state, pos, bytes) in JAVA {
            let data = hex(bytes);
            let mut r = Reader::new(&data);
            let got = read_block(&mut r, true).unwrap();
            assert_eq!(
                got,
                BlockOption {
                    state,
                    pos: Some(pos)
                },
                "{bytes}"
            );
            assert_eq!(r.remaining(), 0);
        }
    }

    #[test]
    fn encodes_the_bytes_fabric_writes() {
        for (state, pos, bytes) in JAVA {
            let mut w = Writer::new();
            write_block(
                &mut w,
                BlockOption {
                    state,
                    pos: Some(pos),
                },
                true,
            );
            assert_eq!(w.data, hex(bytes));
        }
    }

    #[test]
    fn a_vanilla_option_decodes_either_way() {
        // Java: `ByteBufCodecs.VAR_INT.encode(27000)`.
        let data = hex("f8d201");
        for extended in [false, true] {
            let mut r = Reader::new(&data);
            assert_eq!(
                read_block(&mut r, extended).unwrap(),
                BlockOption {
                    state: 27000,
                    pos: None
                }
            );
            assert_eq!(r.remaining(), 0);
        }
    }

    #[test]
    fn without_the_channel_the_position_stays_off_the_wire() {
        let option = BlockOption {
            state: 27000,
            pos: Some([1, 2, 3]),
        };
        let mut w = Writer::new();
        write_block(&mut w, option, false);
        assert_eq!(w.data, hex("f8d201"));
        // And no position: vanilla even when negotiated (`pos == null`).
        let mut w = Writer::new();
        write_block(
            &mut w,
            BlockOption {
                pos: None,
                ..option
            },
            true,
        );
        assert_eq!(w.data, hex("f8d201"));
    }

    #[test]
    fn un_negotiated_the_marker_is_read_as_vanilla_reads_it() {
        // A vanilla client takes the marker for the state id and leaves the
        // rest unread; the position is not taken.
        let data = hex(JAVA[1].2);
        let mut r = Reader::new(&data);
        let got = read_block(&mut r, false).unwrap();
        assert_eq!(
            got,
            BlockOption {
                state: -1,
                pos: None
            }
        );
        assert_eq!(r.remaining(), data.len() - 5);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        let full = hex(JAVA[2].2);
        // Every truncation of the extended form.
        for n in 0..full.len() {
            let mut r = Reader::new(&full[..n]);
            assert!(read_block(&mut r, true).is_err(), "{n} bytes");
        }
        // A VarInt longer than five bytes.
        let mut r = Reader::new(&[0xff; 8]);
        assert!(read_block(&mut r, true).is_err());
        // Two markers: no block state behind them.
        let data = hex("ffffffff0fffffffff0f0000000000000000");
        assert!(read_block(&mut Reader::new(&data), true).is_err());
        // Another negative id after the marker.
        let data = hex("ffffffff0ffeffffff0f0000000000000000");
        assert!(read_block(&mut Reader::new(&data), true).is_err());
    }

    #[test]
    fn a_scope_resets_the_flag_and_restores_it() {
        let outer = Scope::new();
        assert!(!negotiated());
        negotiate();
        assert!(negotiated());
        {
            let _inner = Scope::new();
            assert!(!negotiated(), "a new connection starts un-negotiated");
        }
        assert!(negotiated());
        drop(outer);
        assert!(!negotiated());
    }
}
