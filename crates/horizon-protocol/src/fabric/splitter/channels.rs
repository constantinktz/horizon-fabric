//! The play channels of a Fabric connection, as Fabric API's
//! `AbstractChanneledNetworkAddon` and `ClientPlayNetworkAddon` keep them:
//! the channels the server receives (`sendableChannels`: what Horizon may
//! send it) and the channels Horizon receives (`getReceivableChannels`: what
//! it announces).
//!
//! How they are exchanged (`CommonPacketsImpl`, `ClientNetworkingImpl`):
//! during configuration the server sends `c:register` for the `play` phase
//! with its play channels, and the client answers with its own; those are
//! the pending play channels each side starts play with. Once in play the
//! client sends `minecraft:register` with its channels right after the
//! server's `Login` (`ClientPlayNetworkAddon.onServerReady`), and from then
//! on one more `minecraft:register` / `minecraft:unregister` per channel it
//! adds or drops. The server's own play `minecraft:register` (sent when its
//! player is ready) and `minecraft:unregister` only change what Horizon may
//! send; a client never answers them. `c:register` in play has no receiver
//! on a Fabric client and is ignored.
//!
//! A new configuration (`StartConfiguration`) starts a new play session:
//! [`Channels::reconfigure`] forgets the server's channels and waits for the
//! next `Login` before announcing again.
use super::super::{COMMON_VERSION, PLAY, REGISTER, decode_channels, encode_channels};
use crate::{Error, Result, buf::Reader};
use std::collections::BTreeSet;

/// `RegistrationPayload.UNREGISTER`.
pub const UNREGISTER: &str = "minecraft:unregister";
/// The play channels Horizon receives from a Fabric server. A Fabric module
/// that handles a play payload adds its channel here; nothing else may be
/// listed, as a server sends only what a client announced.
pub const CLIENT_PLAY: &[&str] = &[];
/// `AbstractChanneledNetworkAddon.MAX_CHANNELS`.
pub const MAX_CHANNELS: usize = 8192;
/// `AbstractChanneledNetworkAddon.MAX_CHANNEL_NAME_LENGTH`
/// (`GlobalReceiverRegistry.DEFAULT_CHANNEL_NAME_MAX_LENGTH`).
pub const MAX_CHANNEL_NAME: usize = 128;

/// A decoded `CommonRegisterPayload`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommonRegister {
    pub version: i32,
    /// `play` or `configuration`.
    pub protocol: String,
    pub channels: Vec<String>,
}

/// `CommonRegisterPayload(FriendlyByteBuf)`: a VarInt version, the phase
/// (`readUtf`), then a set of identifiers of at most [`MAX_CHANNELS`]
/// (`ByteBufCodecs.collection`). An identifier `Identifier.parse` refuses
/// fails the payload, as do bytes after it.
pub fn decode_common_register(data: &[u8]) -> Result<CommonRegister> {
    let mut r = Reader::new(data);
    let version = r.varint()?;
    let protocol = r.string(32767)?;
    let n = r.count(MAX_CHANNELS)?;
    let mut channels = Vec::with_capacity(n.min(r.remaining()));
    for _ in 0..n {
        let name = r.string(32767)?;
        let id = normalize(&name)
            .ok_or_else(|| Error::new(format!("Non [a-z0-9_.-:/] character in id: {name}")))?;
        if !channels.contains(&id) {
            channels.push(id);
        }
    }
    if r.remaining() > 0 {
        return Err(Error::new("c:register: trailing bytes"));
    }
    Ok(CommonRegister {
        version,
        protocol,
        channels,
    })
}

/// One connection's play channels.
#[derive(Clone, Debug, Default)]
pub struct Channels {
    /// `sendableChannels`: the channels the server receives.
    server: BTreeSet<String>,
    /// `getReceivableChannels`: the channels Horizon receives.
    client: BTreeSet<String>,
    /// `ClientCommonNetworkAddon.isServerReady`: the server's `Login` came,
    /// so registrations go out as they happen.
    ready: bool,
}

impl Channels {
    /// A connection whose client receives `client`.
    pub fn new<'a>(client: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            server: BTreeSet::new(),
            client: client.into_iter().filter_map(normalize).collect(),
            ready: false,
        }
    }

    /// A connection receiving [`CLIENT_PLAY`].
    pub fn play() -> Self {
        Self::new(CLIENT_PLAY.iter().copied())
    }

    /// Whether the server takes payloads on `channel`
    /// (`ClientPlayNetworking.canSend`).
    pub fn can_send(&self, channel: &str) -> bool {
        normalize(channel).is_some_and(|c| self.server.contains(&c))
    }

    /// The channels the server receives, sorted.
    pub fn server(&self) -> impl Iterator<Item = &str> {
        self.server.iter().map(String::as_str)
    }

    /// The channels Horizon receives, sorted.
    pub fn client(&self) -> impl Iterator<Item = &str> {
        self.client.iter().map(String::as_str)
    }

    /// The configuration `c:register` (`CommonPacketsImpl`'s client
    /// receiver): a `play` one gives the server's pending play channels and
    /// is answered with Horizon's, as a `c:register` payload. A version other
    /// than the agreed one is an error (Fabric's `IllegalStateException`); a
    /// `configuration` one is answered with no channels, so the server keeps
    /// to what `minecraft:register` already told it.
    pub fn receive_common_register(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        let payload = decode_common_register(data)?;
        if payload.version != COMMON_VERSION {
            return Err(Error::new(format!(
                "Negotiated common packet version: {COMMON_VERSION} but received packet with version: {}",
                payload.version
            )));
        }
        if payload.protocol != PLAY {
            return Ok(super::super::encode_common_register(
                COMMON_VERSION,
                &payload.protocol,
                &[],
            ));
        }
        self.register(&payload.channels)?;
        let client: Vec<&str> = self.client().collect();
        Ok(super::super::encode_common_register(
            COMMON_VERSION,
            PLAY,
            &client,
        ))
    }

    /// A play custom payload: the server's `minecraft:register` or
    /// `minecraft:unregister` (`AbstractChanneledNetworkAddon.handle`).
    /// `Ok(false)` for any other channel; an error for a payload Fabric
    /// refuses (too many or too long names), after the names before it
    /// were taken, as in Fabric.
    pub fn receive(&mut self, channel: &str, data: &[u8]) -> Result<bool> {
        let register = match channel {
            REGISTER => true,
            UNREGISTER => false,
            _ => return Ok(false),
        };
        let names: Vec<String> = decode_channels(data)?
            .iter()
            .filter_map(|n| normalize(n))
            .collect();
        if register {
            self.register(&names)?;
        } else {
            for name in &names {
                self.server.remove(name);
            }
        }
        Ok(true)
    }

    /// `registerChannel` for each name: at most [`MAX_CHANNELS`] (counted
    /// before each one, duplicates included) of at most
    /// [`MAX_CHANNEL_NAME`] characters.
    fn register(&mut self, names: &[String]) -> Result<()> {
        for name in names {
            if self.server.len() >= MAX_CHANNELS {
                return Err(Error::new(format!(
                    "Cannot register more than {MAX_CHANNELS} channels"
                )));
            }
            if name.len() > MAX_CHANNEL_NAME {
                return Err(Error::new("Channel name is too long"));
            }
            self.server.insert(name.clone());
        }
        Ok(())
    }

    /// The server's `Login` (`ClientPlayNetworkAddon.onServerReady`): the
    /// `minecraft:register` payload with Horizon's channels, `None` when it
    /// has none (`createRegistrationPayload` sends nothing then).
    pub fn server_ready(&mut self) -> Option<Vec<u8>> {
        self.ready = true;
        self.registration(self.client.iter().map(String::as_str))
    }

    /// Horizon starts receiving `channel` (`handleRegistration`): once the
    /// server is ready, the `minecraft:register` payload naming it.
    pub fn add(&mut self, channel: &str) -> Option<Vec<u8>> {
        let channel = normalize(channel)?;
        let added = self.client.insert(channel.clone());
        if added && self.ready {
            self.registration([channel.as_str()])
        } else {
            None
        }
    }

    /// Horizon stops receiving `channel` (`handleUnregistration`): once the
    /// server is ready, the `minecraft:unregister` payload naming it.
    pub fn remove(&mut self, channel: &str) -> Option<Vec<u8>> {
        let channel = normalize(channel)?;
        let removed = self.client.remove(&channel);
        if removed && self.ready {
            self.registration([channel.as_str()])
        } else {
            None
        }
    }

    /// `StartConfiguration`: the next play session starts from the channels
    /// its configuration exchanges.
    pub fn reconfigure(&mut self) {
        self.server.clear();
        self.ready = false;
    }

    fn registration<'a>(&self, channels: impl IntoIterator<Item = &'a str>) -> Option<Vec<u8>> {
        let channels: Vec<&str> = channels.into_iter().collect();
        (!channels.is_empty()).then(|| encode_channels(&channels))
    }
}

/// `Identifier.tryParse(name).toString()`: `minecraft:` for a name without
/// a namespace (or an empty one), `None` for a character an identifier
/// can't hold.
fn normalize(name: &str) -> Option<String> {
    let (ns, path) = match name.split_once(':') {
        Some(("", path)) => ("minecraft", path),
        Some((ns, path)) => (ns, path),
        None => ("minecraft", name),
    };
    let ok_ns = ns
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b));
    let ok_path = path
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-./".contains(&b));
    (ok_ns && ok_path).then(|| format!("{ns}:{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CommonRegisterPayload.write` by hand: VarInt 1, `writeUtf("play")`,
    /// VarInt 2 channels, each `writeUtf`.
    fn common_register(version: u8, protocol: &str, channels: &[&str]) -> Vec<u8> {
        let mut b = vec![version, protocol.len() as u8];
        b.extend_from_slice(protocol.as_bytes());
        b.push(channels.len() as u8);
        for c in channels {
            b.push(c.len() as u8);
            b.extend_from_slice(c.as_bytes());
        }
        b
    }

    #[test]
    fn the_servers_play_channels_come_with_c_register() {
        let mut ch = Channels::new(["fabric:recipe_sync"]);
        let data = common_register(1, "play", &["fabric:attachment_sync_v1", "mod:hello"]);
        let answer = ch.receive_common_register(&data).unwrap();
        assert_eq!(answer, common_register(1, "play", &["fabric:recipe_sync"]));
        assert!(ch.can_send("mod:hello"));
        assert!(ch.can_send("fabric:attachment_sync_v1"));
        assert!(!ch.can_send("mod:other"));
        // Horizon receives nothing of its own yet: the answer lists none.
        let mut none = Channels::play();
        assert_eq!(
            none.receive_common_register(&data).unwrap(),
            [1, 4, b'p', b'l', b'a', b'y', 0]
        );
    }

    #[test]
    fn c_register_limits() {
        let mut ch = Channels::play();
        // Another version than the agreed one.
        assert!(
            ch.receive_common_register(&common_register(2, "play", &["a:b"]))
                .is_err()
        );
        // An identifier Identifier.parse refuses.
        assert!(decode_common_register(&common_register(1, "play", &["Bad:Name"])).is_err());
        // Truncated and padded.
        let full = common_register(1, "play", &["a:b"]);
        for cut in 0..full.len() {
            assert!(decode_common_register(&full[..cut]).is_err(), "{cut}");
        }
        let mut padded = full.clone();
        padded.push(0);
        assert!(decode_common_register(&padded).is_err());
        // More than MAX_CHANNELS names: VarInt 8193 is refused before any
        // is read.
        let mut many = vec![1, 4];
        many.extend_from_slice(b"play");
        many.extend_from_slice(&[0x81, 0x40]); // 8193
        assert!(decode_common_register(&many).is_err());
        // A name without a namespace is minecraft's, duplicates collapse.
        let got =
            decode_common_register(&common_register(1, "play", &["brand", "minecraft:brand"]))
                .unwrap();
        assert_eq!(got.channels, ["minecraft:brand"]);
    }

    #[test]
    fn the_servers_register_and_unregister_change_what_may_be_sent() {
        let mut ch = Channels::play();
        assert!(ch.receive(REGISTER, b"mod:a\0mod:b\0c").unwrap());
        assert!(ch.can_send("mod:a") && ch.can_send("mod:b"));
        assert!(ch.can_send("minecraft:c") && ch.can_send("c"));
        assert!(ch.receive(UNREGISTER, b"mod:a").unwrap());
        assert!(!ch.can_send("mod:a"));
        assert_eq!(ch.server().collect::<Vec<_>>(), ["minecraft:c", "mod:b"]);
        // Other channels are none of its business.
        assert!(!ch.receive("mod:a", b"mod:x").unwrap());
        assert!(!ch.can_send("mod:x"));
    }

    #[test]
    fn registrations_beyond_fabrics_limits_are_refused() {
        // A name longer than 128 characters once namespaced.
        let mut ch = Channels::play();
        let long = "a".repeat(120);
        assert!(ch.receive(REGISTER, long.as_bytes()).is_err());
        assert!(!ch.can_send(&long));
        // 8193 names in one payload: RegistrationPayload.read refuses it whole.
        let names: Vec<String> = (0..MAX_CHANNELS + 1).map(|i| format!("m:{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        assert!(ch.receive(REGISTER, &encode_channels(&refs)).is_err());
        assert_eq!(ch.server().count(), 0);
        // 8192 are taken; registerChannel refuses the next one.
        assert!(
            ch.receive(REGISTER, &encode_channels(&refs[..MAX_CHANNELS]))
                .is_ok()
        );
        assert!(ch.receive(REGISTER, b"m:more").is_err());
        assert_eq!(ch.server().count(), MAX_CHANNELS);
        assert!(!ch.can_send("m:more"));
        // Raw bytes that aren't names never panic.
        let mut ch = Channels::play();
        for data in [&[0xFF, 0x00, 0xFE][..], b"\0\0\0", b"", b"a:\0:b"] {
            assert!(ch.receive(REGISTER, data).is_ok());
        }
    }

    #[test]
    fn horizon_announces_after_login_then_one_channel_at_a_time() {
        let mut ch = Channels::new(["mod:b", "mod:a"]);
        // Before Login nothing goes out.
        assert_eq!(ch.add("mod:c"), None);
        // RegistrationPayload.write: the names with NUL between them.
        assert_eq!(ch.server_ready().unwrap(), b"mod:a\0mod:b\0mod:c");
        assert_eq!(ch.add("mod:d").unwrap(), b"mod:d");
        assert_eq!(ch.add("mod:d"), None);
        assert_eq!(ch.remove("mod:a").unwrap(), b"mod:a");
        assert_eq!(ch.remove("mod:a"), None);
        // A new configuration: the server's channels are gone, and the next
        // Login announces again.
        ch.receive(REGISTER, b"mod:x").unwrap();
        ch.reconfigure();
        assert!(!ch.can_send("mod:x"));
        assert_eq!(ch.add("mod:e"), None);
        assert_eq!(ch.server_ready().unwrap(), b"mod:b\0mod:c\0mod:d\0mod:e");
        // Nothing to announce: no packet at all.
        assert_eq!(Channels::play().server_ready(), None);
    }
}
