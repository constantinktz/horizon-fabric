//! Fabric API menu-api-v1: extended menu opening data
//! (`impl.menu.Networking.OpenScreenPayload`, Fabric API 0.162).
//!
//! A mod's `ExtendedMenuType` carries data its client half needs to build the
//! menu (often the block's `BlockPos`). For such a menu the server sends no
//! `ClientboundOpenScreenPacket` at all: `ServerPlayerMixin` redirects
//! `openMenu`'s send to `Networking.sendOpenPacket`, which sends this payload
//! instead, on the play channel [`OPEN_SCREEN`]. Fabric sends it whether or
//! not the client registered the channel (`ServerPlayNetworking.send` does
//! not check `canSend`), so a client that ignores it is left with a menu the
//! server thinks is open. The `ContainerSetContent` of the same window follows
//! as for any menu (`initMenu`).
//!
//! Layout: the menu type's identifier, the container id as one byte
//! (`readByte`: signed), the title (`ComponentSerialization.STREAM_CODEC`:
//! network NBT under a 2 MiB accounter), then the bytes of the type's own
//! `StreamCodec`, which only its mod can read: kept here as they are.
use crate::{Error, Result, buf::Reader};
use horizon_world::nbt::Tag;

/// `Networking.OPEN_ID`.
pub const OPEN_SCREEN: &str = "fabric-menu-api-v1:open_screen";
/// `ClientboundCustomPayloadPacket.MAX_PAYLOAD_SIZE`: no payload is longer.
pub const MAX_PAYLOAD: usize = 1_048_576;
/// `FriendlyByteBuf.readIdentifier`'s `readUtf` limit.
const MAX_IDENTIFIER: usize = 32767;

/// An `OpenScreenPayload` as it arrives.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenScreen {
    /// The menu type's id in the server's `minecraft:menu` registry,
    /// `namespace:path`.
    pub menu: String,
    /// The window id (`containerCounter`), as the `OpenScreen` packet's.
    pub container: i32,
    /// The title component, in the same network NBT as `OpenScreen.title`.
    pub title: Tag,
    /// The menu type's own opening data (`ExtendedMenuType.getStreamCodec`).
    pub data: Vec<u8>,
}

/// `OpenScreenPayload.fromBuf`, up to the type's own data, which is kept raw.
/// An error for a bad identifier, a truncated or missing title, or a payload
/// past [`MAX_PAYLOAD`].
pub fn decode_open_screen(data: &[u8]) -> Result<OpenScreen> {
    if data.len() > MAX_PAYLOAD {
        return Err(Error::new("fabric open_screen: payload too large"));
    }
    let mut r = Reader::new(data);
    let id = r.string(MAX_IDENTIFIER)?;
    // `Identifier.parse`: a bare path is in `minecraft`.
    if !super::identifier_ok(&id) {
        return Err(Error::new("fabric open_screen: bad menu identifier"));
    }
    let menu = if id.contains(':') {
        id
    } else {
        format!("minecraft:{id}")
    };
    let container = i32::from(r.i8()?);
    let title = r.nbt()?;
    // `ComponentSerialization.CODEC` has no form an end tag decodes to.
    if title == Tag::End {
        return Err(Error::new("fabric open_screen: no title"));
    }
    Ok(OpenScreen {
        menu,
        container,
        title,
        data: r.rest().to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `OpenScreenPayload.write` puts on the wire for the testmod's
    /// `BOX_SCREEN_MENU` (`ExtendedMenuType(BoxMenu::new,
    /// BlockPos.STREAM_CODEC)`), window 7, title `Component.literal("Box")`,
    /// box at (1, 64, -2): written out by hand from the codecs.
    fn box_payload() -> Vec<u8> {
        let id = b"fabric-menu-api-v1-testmod:box";
        let mut b = vec![id.len() as u8];
        b.extend_from_slice(id);
        // writeByte(containerId)
        b.push(7);
        // A plain literal collapses to a string tag: TAG_String, u16 length.
        b.extend_from_slice(&[8, 0, 3, b'B', b'o', b'x']);
        // BlockPos.asLong: x << 38 | (z & 0x3FFFFFF) << 12 | (y & 0xFFF).
        b.extend_from_slice(&[0x00, 0x00, 0x00, 0x7F, 0xFF, 0xFF, 0xE0, 0x40]);
        b
    }

    #[test]
    fn the_testmod_box_decodes() {
        let p = decode_open_screen(&box_payload()).unwrap();
        assert_eq!(p.menu, "fabric-menu-api-v1-testmod:box");
        assert_eq!(p.container, 7);
        match &p.title {
            Tag::String(s) => assert_eq!(s.to_string().unwrap(), "Box"),
            t => panic!("title {t:?}"),
        }
        assert_eq!(p.data, [0x00, 0x00, 0x00, 0x7F, 0xFF, 0xFF, 0xE0, 0x40]);
        assert_eq!(
            crate::buf::unpack_block_pos(i64::from_be_bytes(p.data[..].try_into().unwrap())),
            [1, 64, -2]
        );
    }

    #[test]
    fn a_compound_title_and_no_data() {
        // `{"translate":"container.chest"}`: TAG_Compound, one string entry
        // "translate", end. A menu type whose codec writes nothing (`Unit`).
        let mut b = vec![7];
        b.extend_from_slice(b"mod:bag");
        b.push(0x80); // container id 128 as a byte: -128, as Java reads it
        b.push(10);
        b.extend_from_slice(&[8, 0, 9]);
        b.extend_from_slice(b"translate");
        b.extend_from_slice(&[0, 15]);
        b.extend_from_slice(b"container.chest");
        b.push(0);
        let p = decode_open_screen(&b).unwrap();
        assert_eq!(p.menu, "mod:bag");
        assert_eq!(p.container, -128);
        assert!(matches!(p.title, Tag::Compound(_)));
        assert!(p.data.is_empty());
    }

    #[test]
    fn a_bare_path_is_a_minecraft_id() {
        let mut b = vec![3];
        b.extend_from_slice(b"bag");
        b.extend_from_slice(&[1, 8, 0, 0]);
        assert_eq!(decode_open_screen(&b).unwrap().menu, "minecraft:bag");
    }

    #[test]
    fn malformed_payloads_are_errors_not_panics() {
        let good = box_payload();
        // Every truncation before the title ends.
        let title_end = good.len() - 8;
        for n in 0..title_end {
            assert!(decode_open_screen(&good[..n]).is_err(), "prefix {n}");
        }
        // Past it the rest is the mod's data, whatever its length.
        for n in title_end..=good.len() {
            assert_eq!(
                decode_open_screen(&good[..n]).unwrap().data.len(),
                n - title_end
            );
        }
        // Not an identifier (`Identifier.parse` throws).
        let mut bad = vec![7];
        bad.extend_from_slice(b"Mod:Bag");
        bad.extend_from_slice(&[1, 8, 0, 0]);
        assert!(decode_open_screen(&bad).is_err());
        // An end tag where the title goes.
        let mut end = vec![7];
        end.extend_from_slice(b"mod:bag");
        end.extend_from_slice(&[1, 0]);
        assert!(decode_open_screen(&end).is_err());
        // A string length past the payload, a negative one, a huge one.
        for head in [
            &[0x7F][..],
            &[0xFF, 0xFF, 0xFF, 0xFF, 0x0F],
            &[0xFF, 0xFF, 0x7F],
        ] {
            assert!(decode_open_screen(head).is_err());
        }
        // An unknown tag type in the title.
        let mut tag = vec![7];
        tag.extend_from_slice(b"mod:bag");
        tag.extend_from_slice(&[1, 13, 0]);
        assert!(decode_open_screen(&tag).is_err());
        // Bigger than any custom payload.
        assert!(decode_open_screen(&vec![0; MAX_PAYLOAD + 1]).is_err());
        // Random bytes never panic.
        let mut x: u32 = 0x1234_5678;
        for _ in 0..2000 {
            let len = (x % 64) as usize;
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                buf.push(x as u8);
            }
            let _ = decode_open_screen(&buf);
        }
    }
}
