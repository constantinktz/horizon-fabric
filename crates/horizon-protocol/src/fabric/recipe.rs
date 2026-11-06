//! Fabric API recipe-api-v1: recipe sync (`RecipeSyncImpl`,
//! `ServerboundSupportedRecipeSerializersPayload`, `ClientboundRecipeSyncPayload`,
//! the client's `ClientConfigurationPacketListenerImplMixin`).
//!
//! Since 1.21.2 a server keeps its recipes to itself. Mods that need some on
//! the client opt their serializers in (`RecipeSynchronization`); the client
//! names the opted-in serializers it has after `SelectKnownPacks`, if the
//! server receives [`SUPPORTED_SERIALIZERS`] and the set isn't empty, and the
//! server then sends their recipes in play on [`RECIPE_SYNC`] (to a client
//! that registered that play channel).
//!
//! Horizon claims no serializer ([`SUPPORTED`]): nothing in Horizon reads
//! recipes on the client (the recipe book uses vanilla's displays), and Fabric
//! API opts in no serializer of its own, so a Fabric client without such mods
//! sends nothing either. [`decode_recipe_sync`] still reads the envelope, so
//! a payload that arrives anyway is skipped as Fabric skips it.
use super::ingredient::{read_identifier, write_identifiers};
use crate::{
    Error, Result,
    buf::{Reader, Writer},
};

/// `ServerboundSupportedRecipeSerializersPayload.TYPE`, a configuration channel.
pub const SUPPORTED_SERIALIZERS: &str = "fabric:recipe_sync/supported_serializers";
/// `ClientboundRecipeSyncPayload.TYPE`, a play channel.
pub const RECIPE_SYNC: &str = "fabric:recipe_sync";
/// `ServerboundSupportedRecipeSerializersPayload.MAX_SYNCHRONIZED_SERIALIZERS`.
pub const MAX_SYNCHRONIZED_SERIALIZERS: usize = 8192;
/// `RecipeSyncImpl.RECIPE_PAYLOAD_MAX_SIZE`: 64 MiB.
pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
/// The recipe serializers whose stream codecs Horizon decodes: none.
pub const SUPPORTED: [&str; 0] = [];

/// `ServerboundSupportedRecipeSerializersPayload.CODEC`: the serializer ids.
pub fn encode_supported(serializers: &[&str]) -> Vec<u8> {
    let mut w = Writer::new();
    write_identifiers(&mut w, serializers);
    w.data
}

/// What the client sends after answering `SelectKnownPacks`, if anything:
/// only to a server that receives the channel (its `minecraft:register`),
/// and never an empty set ("it's the default state anyway").
pub fn request(server_channels: &[String]) -> Option<Vec<u8>> {
    request_for(server_channels, &SUPPORTED)
}

fn request_for(server_channels: &[String], supported: &[&str]) -> Option<Vec<u8>> {
    let receives = server_channels.iter().any(|c| c == SUPPORTED_SERIALIZERS);
    (receives && !supported.is_empty()).then(|| encode_supported(supported))
}

/// `ClientboundRecipeSyncPayload.Entry.read`, up to the first serializer:
/// `Ok` for an empty list (no synced recipes, Fabric's `EMPTY`), and an error
/// for any entry, since its serializer is never one Horizon claimed. Fabric
/// throws `SkipPacketDecoderException` there: the packet is dropped, the
/// connection stays.
pub fn decode_recipe_sync(data: &[u8]) -> Result<()> {
    if data.len() > MAX_PAYLOAD {
        return Err(Error::new("recipe sync payload is larger than 64 MiB"));
    }
    let mut r = Reader::new(data);
    let entries = r.count(r.remaining())?;
    if entries == 0 {
        return Ok(());
    }
    let serializer = read_identifier(&mut r)?;
    Err(Error::new(format!(
        "Tried syncing unsupported packet serializer '{serializer}'!"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// The payload is `ByteBufCodecs.collection(Identifier.STREAM_CODEC)`:
    /// VarInt count, then each identifier's VarInt length and UTF-8.
    #[test]
    fn supported_serializers_are_a_collection_of_identifiers() {
        let mut expected = vec![0x01, 25];
        expected.extend_from_slice(b"minecraft:crafting_shaped");
        assert_eq!(encode_supported(&["minecraft:crafting_shaped"]), expected);
        assert_eq!(encode_supported(&[]), vec![0x00]);
    }

    /// Horizon sends nothing: it claims no serializer, and even one claimed
    /// is only sent to a server that registered the channel.
    #[test]
    fn the_request_needs_the_servers_channel_and_a_serializer() {
        let fabric = channels(&["c:version", SUPPORTED_SERIALIZERS]);
        assert_eq!(request(&fabric), None);
        assert_eq!(request_for(&channels(&["c:version"]), &["a:b"]), None);
        assert_eq!(request_for(&fabric, &[]), None);
        assert_eq!(
            request_for(&fabric, &["a:b"]),
            Some(vec![0x01, 0x03, b'a', b':', b'b'])
        );
    }

    /// An empty entry list is Fabric's `SynchronizedRecipesImpl.EMPTY`.
    #[test]
    fn an_empty_recipe_sync_is_no_recipes() {
        assert!(decode_recipe_sync(&[0x00]).is_ok());
    }

    /// An entry: the serializer id, the recipe count, each recipe's key and
    /// body. Horizon stops at the id it never claimed and names it.
    #[test]
    fn a_recipe_sync_entry_is_skipped_by_its_serializer() {
        let mut data = vec![0x01, 25];
        data.extend_from_slice(b"minecraft:crafting_shaped");
        data.extend_from_slice(&[0x01, 9]);
        data.extend_from_slice(b"mod:thing");
        let err = decode_recipe_sync(&data).unwrap_err();
        assert!(
            err.to_string().contains("minecraft:crafting_shaped"),
            "{err}"
        );
    }

    #[test]
    fn malformed_recipe_syncs_are_errors() {
        assert!(decode_recipe_sync(&[]).is_err());
        // A count larger than the payload, a negative one.
        assert!(decode_recipe_sync(&[0x05]).is_err());
        assert!(decode_recipe_sync(&[0xff, 0xff, 0xff, 0xff, 0x0f]).is_err());
        // An entry with a truncated or invalid serializer id.
        assert!(decode_recipe_sync(&[0x01, 0x09, b'a']).is_err());
        let mut data = vec![0x01, 3];
        data.extend_from_slice(b"A:B");
        assert!(decode_recipe_sync(&data).is_err());
        // Past Fabric's 64 MiB limit, even an empty list.
        let mut big = vec![0u8; MAX_PAYLOAD + 1];
        big[0] = 0x00;
        assert!(decode_recipe_sync(&big).is_err());
    }
}
