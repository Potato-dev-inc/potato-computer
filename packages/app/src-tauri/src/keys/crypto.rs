//! Ed25519 keys and base64 helpers used by the PC link.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use rand::RngCore;

pub use ed25519_dalek::SigningKey;

pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

pub fn unb64(text: &str) -> Option<Vec<u8>> {
    B64.decode(text.trim()).ok()
}

pub fn unb64_32(text: &str) -> Option<[u8; 32]> {
    unb64(text)?.try_into().ok()
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut out);
    out
}

pub fn new_signing_key() -> SigningKey {
    SigningKey::from_bytes(&random_bytes::<32>())
}
