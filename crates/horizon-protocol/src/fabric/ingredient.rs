//! Fabric API recipe-api-v1: custom ingredient sync (`fabric:custom_ingredient_sync`,
//! `CustomIngredientSync`, `ClientboundCustomIngredientPayload`,
//! `ServerboundCustomIngredientPayload`, `CustomIngredientStreamCodec`).
//!
//! A Fabric server asks a client that registered [`CHANNEL`] which custom
//! ingredient serializers it knows (a configuration task, answered once).
//! Where an `Ingredient` crosses the network (`RecipeDisplayEntry`'s crafting
//! requirements, the stonecutter's `SelectableRecipe`), the server then writes
//! an ingredient the client knows as the marker [`PACKET_MARKER`], the
//! serializer id and its own stream codec, and every other one as the vanilla
//! `HolderSet` of the items it matches (the fallback).
//!
//! Horizon claims no custom ingredient ([`SUPPORTED`]): the fallback already
//! names the items each slot accepts, which is all the recipe book shows and
//! filters on, and the matching itself happens on the server. A client that
//! doesn't register the channel, or answers with an empty set, always gets the
//! fallback (`CustomIngredientStreamCodec.shouldEncodeFallback`).
use super::identifier_ok;
use crate::{
    Error, Result,
    buf::{Reader, Writer},
    packets::recipes::HolderSet,
};

/// `CustomIngredientSync.PACKET_ID`, the same in both directions.
pub const CHANNEL: &str = "fabric:custom_ingredient_sync";
/// `CustomIngredientSync.PROTOCOL_VERSION_1`.
pub const PROTOCOL_VERSION_1: i32 = 1;
/// `CustomIngredientStreamCodec.PACKET_MARKER`: a custom ingredient follows.
pub const PACKET_MARKER: i32 = -1;
/// The custom ingredient serializers Horizon decodes: none (see the module
/// docs). Fabric API's own are `fabric:all_of`, `fabric:any_of`,
/// `fabric:difference`, `fabric:components` and `fabric:custom_data`.
pub const SUPPORTED: [&str; 0] = [];

/// `ServerboundCustomIngredientPayload.MAX_REGISTERED_SERIALIZERS`.
pub const MAX_REGISTERED_SERIALIZERS: usize = 8192;
/// `FriendlyByteBuf.readUtf`'s default limit, which `Identifier.STREAM_CODEC` uses.
const MAX_IDENTIFIER: usize = 32767;

/// `ClientboundCustomIngredientPayload.CODEC`: the server's protocol version.
pub fn decode_query(data: &[u8]) -> Result<i32> {
    Reader::new(data).varint()
}

/// `CustomIngredientSync.createResponsePayload`: always protocol 1 with the
/// serializers Horizon knows; nothing for a version below 1 (Fabric then has
/// no answer either).
pub fn response(server_version: i32) -> Option<Vec<u8>> {
    (server_version >= PROTOCOL_VERSION_1).then(|| encode_response(PROTOCOL_VERSION_1, &SUPPORTED))
}

/// `ServerboundCustomIngredientPayload.CODEC`: the version, then the
/// serializer ids as a collection of identifiers.
pub fn encode_response(version: i32, serializers: &[&str]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(version);
    write_identifiers(&mut w, serializers);
    w.data
}

/// `ByteBufCodecs.collection(Identifier.STREAM_CODEC)`: a VarInt count, then
/// each identifier as a string.
pub(super) fn write_identifiers(w: &mut Writer, ids: &[&str]) {
    w.varint(ids.len() as i32);
    for id in ids {
        w.string(id);
    }
}

/// `FriendlyByteBuf.readIdentifier`: a string `Identifier.parse` accepts.
pub(super) fn read_identifier(r: &mut Reader) -> Result<String> {
    let s = r.string(MAX_IDENTIFIER)?;
    if !identifier_ok(&s) {
        return Err(Error::new(format!(
            "Non [a-z0-9_.-] character in identifier: {s}"
        )));
    }
    Ok(s)
}

/// `Ingredient.CONTENTS_STREAM_CODEC` as Fabric replaces it: the marker and
/// a custom ingredient's serializer id, or else the vanilla `HolderSet`
/// (whose first VarInt is the one already read). Horizon knows no custom
/// ingredient, so that is an error naming the type, as Fabric's own "Cannot
/// deserialize custom ingredient of unknown type".
pub fn read_ingredient(r: &mut Reader) -> Result<HolderSet> {
    let head = r.varint()?;
    if head == PACKET_MARKER {
        let kind = read_identifier(r)?;
        return Err(Error::new(format!(
            "Cannot deserialize custom ingredient of unknown type {kind}"
        )));
    }
    // HolderSet.read: 0 is a tag by name, n the n - 1 item ids that follow.
    if head == 0 {
        return Ok(HolderSet::Tag(r.string(MAX_IDENTIFIER)?));
    }
    let n =
        usize::try_from(i64::from(head) - 1).map_err(|_| Error::new("negative holder set size"))?;
    if n > r.remaining() {
        return Err(Error::new("holder set larger than the packet"));
    }
    Ok(HolderSet::Items(
        (0..n)
            .map(|_| r.varint().map(crate::adapter::ids::item))
            .collect::<Result<_>>()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ClientboundCustomIngredientPayload(1)`: one VarInt.
    #[test]
    fn the_query_is_the_servers_protocol_version() {
        assert_eq!(decode_query(&[0x01]).unwrap(), 1);
        assert_eq!(decode_query(&[0x80, 0x01]).unwrap(), 128);
        assert!(decode_query(&[]).is_err());
        assert!(decode_query(&[0x80]).is_err());
        assert!(decode_query(&[0xff, 0xff, 0xff, 0xff, 0xff, 0x01]).is_err());
    }

    /// `ServerboundCustomIngredientPayload(1, {})`: VarInt 1, an empty
    /// collection; a newer server still gets protocol 1, an older one nothing.
    #[test]
    fn horizon_answers_protocol_one_with_no_serializers() {
        assert_eq!(response(1), Some(vec![0x01, 0x00]));
        assert_eq!(response(7), Some(vec![0x01, 0x00]));
        assert_eq!(response(0), None);
        assert_eq!(response(-5), None);
    }

    /// What a Fabric client with two of Fabric API's serializers sends, laid
    /// out by hand from the codec: VarInt version, VarInt count, then each
    /// identifier as a VarInt length and its UTF-8.
    #[test]
    fn a_response_lists_identifiers_as_strings() {
        let mut expected = vec![0x01, 0x02, 13];
        expected.extend_from_slice(b"fabric:all_of");
        expected.push(17);
        expected.extend_from_slice(b"fabric:difference");
        assert_eq!(
            encode_response(1, &["fabric:all_of", "fabric:difference"]),
            expected
        );
    }

    fn marker() -> Vec<u8> {
        vec![0xff, 0xff, 0xff, 0xff, 0x0f] // VarInt -1
    }

    /// The fallback is the vanilla `HolderSet`: 0 and a tag name, or the size
    /// plus one and the item ids; what follows it is left unread.
    #[test]
    fn a_vanilla_ingredient_reads_as_its_holder_set() {
        let mut data = vec![0x00, 11];
        data.extend_from_slice(b"minecraft:a");
        data.push(0x2a);
        let mut r = Reader::new(&data);
        assert_eq!(
            read_ingredient(&mut r).unwrap(),
            HolderSet::Tag("minecraft:a".into())
        );
        assert_eq!(r.varint().unwrap(), 0x2a);

        let data = [0x03, 0x05, 0x86, 0x01, 0x2a];
        let mut r = Reader::new(&data);
        assert_eq!(
            read_ingredient(&mut r).unwrap(),
            HolderSet::Items(vec![5, 134])
        );
        assert_eq!(r.varint().unwrap(), 0x2a);
    }

    /// A custom ingredient Horizon didn't claim names its type in the error
    /// instead of being misread as a holder set.
    #[test]
    fn a_custom_ingredient_is_an_error_naming_its_type() {
        let mut data = marker();
        data.push(13);
        data.extend_from_slice(b"fabric:any_of");
        data.extend_from_slice(&[0x02, 0x01, 0x01]);
        let err = read_ingredient(&mut Reader::new(&data)).unwrap_err();
        assert!(err.to_string().contains("fabric:any_of"), "{err}");
    }

    #[test]
    fn malformed_ingredients_are_errors() {
        // The marker and nothing after it.
        assert!(read_ingredient(&mut Reader::new(&marker())).is_err());
        // The marker and an identifier Identifier.parse refuses.
        let mut data = marker();
        data.push(6);
        data.extend_from_slice(b"Fab:AB");
        assert!(read_ingredient(&mut Reader::new(&data)).is_err());
        // The marker and a string length past the end.
        let mut data = marker();
        data.extend_from_slice(&[0xff, 0xff, 0x01]);
        assert!(read_ingredient(&mut Reader::new(&data)).is_err());
        // A holder set larger than what's left, a negative size (-2 + 1), nothing.
        assert!(read_ingredient(&mut Reader::new(&[0x7f, 0x01])).is_err());
        assert!(read_ingredient(&mut Reader::new(&[0xfe, 0xff, 0xff, 0xff, 0x0f])).is_err());
        assert!(read_ingredient(&mut Reader::new(&[])).is_err());
        // i32::MIN: head - 1 must not overflow.
        assert!(read_ingredient(&mut Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x08])).is_err());
    }
}
