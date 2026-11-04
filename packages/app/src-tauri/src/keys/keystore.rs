//! Device keys for the PC link, kept in the OS credential store.

use super::crypto::{b64, new_signing_key, unb64_32, SigningKey};
use crate::secrets::SecretBackend;
use std::sync::Arc;

#[derive(Clone)]
pub struct KeyStore {
    backend: Arc<dyn SecretBackend>,
    prefix: String,
}

impl std::fmt::Debug for KeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KeyStore({})", self.prefix)
    }
}

struct Global;

impl SecretBackend for Global {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        crate::secrets::backend().get(name)
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        crate::secrets::backend().set(name, value)
    }
}

impl KeyStore {
    pub fn app() -> Self {
        let profile = crate::app_paths::profile().unwrap_or_else(|| "default".into());
        Self { backend: Arc::new(Global), prefix: format!("pc-link/{profile}/") }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_backend(backend: Arc<dyn SecretBackend>, prefix: &str) -> Self {
        Self { backend, prefix: format!("pc-link/{prefix}/") }
    }

    fn name(&self, what: &str) -> String {
        format!("{}{what}", self.prefix)
    }

    fn get32(&self, what: &str) -> Result<Option<[u8; 32]>, String> {
        Ok(self.backend.get(&self.name(what))?.and_then(|v| unb64_32(&v)))
    }

    fn set32(&self, what: &str, value: &[u8; 32]) -> Result<(), String> {
        self.backend.set(&self.name(what), &b64(value))
    }

    fn get_or_make(&self, what: &str, make: impl FnOnce() -> [u8; 32]) -> Result<[u8; 32], String> {
        if let Some(v) = self.get32(what)? {
            return Ok(v);
        }
        let v = make();
        self.set32(what, &v)?;
        self.get32(what)?.filter(|back| back == &v).ok_or_else(|| "the credential store did not keep the key".into())
    }

    pub fn device_sign(&self) -> Result<SigningKey, String> {
        self.get_or_make("device-sign", || new_signing_key().to_bytes()).map(|b| SigningKey::from_bytes(&b))
    }

    pub fn p2p_node(&self) -> Result<SigningKey, String> {
        self.get_or_make("p2p-node", || new_signing_key().to_bytes()).map(|b| SigningKey::from_bytes(&b))
    }
}
