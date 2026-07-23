use std::fmt;

use bincode::Options;
use rand::{Rng, distributions::Alphanumeric};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const CODE_LEN: usize = 6;
pub const RENDEZVOUS_LEN: usize = 2;
pub const MAX_RELAY_PACKET: usize = 512 * 1024;
pub const FILE_CHUNK_SIZE: usize = 48 * 1024;
pub const MAX_FILE_SIZE: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub [u8; 16]);

impl PeerId {
    pub fn random() -> Self {
        Self(rand::random())
    }

    pub fn short(&self) -> String {
        hex::encode(&self.0[..3])
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PeerId").field(&self.short()).finish()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RelayCommand {
    Host {
        version: u16,
        rendezvous: String,
        max_guests: usize,
    },
    Join {
        version: u16,
        rendezvous: String,
    },
    Data {
        to: Option<PeerId>,
        payload: Vec<u8>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RelayEvent {
    Registered { id: PeerId, is_host: bool },
    PeerJoined { id: PeerId },
    PeerLeft { id: PeerId },
    Data { from: PeerId, payload: Vec<u8> },
    RoomClosed,
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WirePacket {
    Spake { response: bool, message: Vec<u8> },
    PairSealed(Sealed),
    GroupSealed(Sealed),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Sealed {
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Welcome {
    pub room_key: [u8; 32],
    pub host_alias: String,
    pub peer_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppFrame {
    Chat {
        alias: String,
        text: String,
        sent_at_ms: u64,
    },
    Joined {
        alias: String,
    },
    Left {
        alias: String,
    },
    FileOffer {
        id: [u8; 16],
        alias: String,
        name: String,
        size: u64,
        mime: String,
    },
    FileChunk {
        id: [u8; 16],
        sequence: u32,
        bytes: Vec<u8>,
    },
    FileComplete {
        id: [u8; 16],
        sha256: [u8; 32],
    },
    VoiceStart {
        alias: String,
    },
    VoicePcm {
        sample_rate: u32,
        samples: Vec<i16>,
    },
    VoiceStop {
        alias: String,
    },
}

pub fn generate_code() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(CODE_LEN)
        .map(char::from)
        .collect()
}

pub fn validate_code(code: &str) -> bool {
    code.len() == CODE_LEN && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

pub fn rendezvous_id(code: &str) -> String {
    code.chars().take(RENDEZVOUS_LEN).collect()
}

pub fn validate_rendezvous(value: &str) -> bool {
    value.len() == RENDEZVOUS_LEN && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

pub fn encode<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    Ok(bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_RELAY_PACKET as u64)
        .serialize(value)?)
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> anyhow::Result<T> {
    Ok(bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_RELAY_PACKET as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_codes_are_valid_and_random_shaped() {
        for _ in 0..100 {
            let code = generate_code();
            assert!(validate_code(&code));
        }
    }

    #[test]
    fn rejects_malformed_codes() {
        assert!(!validate_code("abc"));
        assert!(!validate_code("abcdef!"));
        assert!(!validate_code("ébc123"));
        assert!(validate_code("aB3xY9"));
        assert_eq!(rendezvous_id("aB3xY9"), "aB");
        assert!(validate_rendezvous("aB"));
        assert!(!validate_rendezvous("aB3"));
    }
}
