use anyhow::{anyhow, bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{AeadCore, ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

const NONCE_LEN: usize = 12;

pub struct MasterKey {
    cipher: ChaCha20Poly1305,
}

impl MasterKey {
    pub fn from_hex(hex64: &str) -> Result<Self> {
        let raw = hex::decode(hex64.trim()).context("VAULTKEEPER_MASTER_KEY is not valid hex")?;
        if raw.len() != 32 {
            bail!(
                "VAULTKEEPER_MASTER_KEY must be 32 bytes (64 hex chars), got {}",
                raw.len()
            );
        }
        let hk = Hkdf::<Sha256>::new(None, &raw);
        let mut okm = [0u8; 32];
        hk.expand(b"vaultkeeper-credentials-v1", &mut okm)
            .map_err(|_| anyhow!("hkdf expand failed"))?;
        Ok(Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(&okm)),
        })
    }

    pub fn from_env() -> Result<Self> {
        let hex64 =
            std::env::var("VAULTKEEPER_MASTER_KEY").context("VAULTKEEPER_MASTER_KEY is not set")?;
        Self::from_hex(&hex64)
    }

    pub fn seal(&self, plaintext: &[u8]) -> Vec<u8> {
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = self
            .cipher
            .encrypt(&nonce, plaintext)
            .expect("encryption cannot fail");
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ct);
        blob
    }

    pub fn open(&self, blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() <= NONCE_LEN {
            bail!("credential blob too short");
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        self.cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| anyhow!("credential decryption failed (wrong master key or corrupt blob)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const K1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const K2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn roundtrip() {
        let k = MasterKey::from_hex(K1).unwrap();
        let blob = k.seal(b"hunter2");
        assert_eq!(k.open(&blob).unwrap(), b"hunter2");
    }

    #[test]
    fn tampered_blob_fails() {
        let k = MasterKey::from_hex(K1).unwrap();
        let mut blob = k.seal(b"hunter2");
        let last = blob.len() - 1;
        blob[last] ^= 1;
        assert!(k.open(&blob).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let blob = MasterKey::from_hex(K1).unwrap().seal(b"x");
        assert!(MasterKey::from_hex(K2).unwrap().open(&blob).is_err());
    }

    #[test]
    fn rejects_short_key() {
        assert!(MasterKey::from_hex("abcd").is_err());
    }
}
