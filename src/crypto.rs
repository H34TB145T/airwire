use anyhow::{Context, Result, bail};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::protocol::{Sealed, decode, encode};

const PAIR_CONTEXT: &[u8] = b"airwire/v1/pair";
const GROUP_CONTEXT: &[u8] = b"airwire/v1/group";

pub fn random_key() -> [u8; 32] {
    let mut key = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

pub fn normalize_spake_key(shared: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"airwire/v1/spake-key");
    hasher.update(shared);
    Zeroizing::new(hasher.finalize().into())
}

pub fn seal_pair<T: serde::Serialize>(key: &[u8; 32], value: &T) -> Result<Sealed> {
    seal(key, PAIR_CONTEXT, value)
}

pub fn open_pair<T: serde::de::DeserializeOwned>(key: &[u8; 32], sealed: &Sealed) -> Result<T> {
    open(key, PAIR_CONTEXT, sealed)
}

pub fn seal_group<T: serde::Serialize>(
    key: &[u8; 32],
    sender: &[u8; 16],
    value: &T,
) -> Result<Sealed> {
    let mut aad = Vec::with_capacity(GROUP_CONTEXT.len() + sender.len());
    aad.extend_from_slice(GROUP_CONTEXT);
    aad.extend_from_slice(sender);
    seal(key, &aad, value)
}

pub fn open_group<T: serde::de::DeserializeOwned>(
    key: &[u8; 32],
    sender: &[u8; 16],
    sealed: &Sealed,
) -> Result<T> {
    let mut aad = Vec::with_capacity(GROUP_CONTEXT.len() + sender.len());
    aad.extend_from_slice(GROUP_CONTEXT);
    aad.extend_from_slice(sender);
    open(key, &aad, sealed)
}

fn seal<T: serde::Serialize>(key: &[u8; 32], aad: &[u8], value: &T) -> Result<Sealed> {
    let plaintext = Zeroizing::new(encode(value)?);
    let cipher = XChaCha20Poly1305::new_from_slice(key).expect("32-byte key");
    let mut nonce = [0_u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_slice(),
                aad,
            },
        )
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    Ok(Sealed { nonce, ciphertext })
}

fn open<T: serde::de::DeserializeOwned>(key: &[u8; 32], aad: &[u8], sealed: &Sealed) -> Result<T> {
    if sealed.ciphertext.len() < 16 {
        bail!("encrypted packet is too short");
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key).expect("32-byte key");
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(&sealed.nonce),
                Payload {
                    msg: &sealed.ciphertext,
                    aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("packet authentication failed"))?,
    );
    decode(&plaintext).context("invalid encrypted payload")
}

#[cfg(test)]
mod tests {
    use super::*;
    use spake2::{Ed25519Group, Identity, Password, Spake2};

    #[test]
    fn group_packets_are_authenticated_to_sender() {
        let key = random_key();
        let sender = [7; 16];
        let other = [8; 16];
        let packet = seal_group(&key, &sender, &"hello").unwrap();
        let opened: String = open_group(&key, &sender, &packet).unwrap();
        assert_eq!(opened, "hello");
        assert!(open_group::<String>(&key, &other, &packet).is_err());
    }

    #[test]
    fn tampering_is_rejected() {
        let key = random_key();
        let mut packet = seal_pair(&key, &42_u64).unwrap();
        packet.ciphertext[0] ^= 1;
        assert!(open_pair::<u64>(&key, &packet).is_err());
    }

    #[test]
    fn wrong_room_code_cannot_open_pair_channel() {
        let identity = Identity::new(b"airwire-room-v1");
        let (host, host_message) =
            Spake2::<Ed25519Group>::start_symmetric(&Password::new(b"aB3xY9"), &identity);
        let (guest, guest_message) =
            Spake2::<Ed25519Group>::start_symmetric(&Password::new(b"aB3xNO"), &identity);
        let host_key = normalize_spake_key(&host.finish(&guest_message).unwrap());
        let guest_key = normalize_spake_key(&guest.finish(&host_message).unwrap());
        assert_ne!(*host_key, *guest_key);
        let packet = seal_pair(&host_key, &"room key").unwrap();
        assert!(open_pair::<String>(&guest_key, &packet).is_err());
    }
}
