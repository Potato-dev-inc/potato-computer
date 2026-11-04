//! The OS credential store used for the PC link keys.

#[cfg(test)]
use parking_lot::Mutex;
#[cfg(test)]
use std::collections::HashMap;

pub const SERVICE: &str = "com.puppetmaster.app";

pub trait SecretBackend: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
}

pub struct KeyringBackend;

impl KeyringBackend {
    fn entry(name: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, name).map_err(|e| e.to_string())
    }
}

impl SecretBackend for KeyringBackend {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        match Self::entry(name)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        Self::entry(name)?.set_password(value).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryBackend {
    values: Mutex<HashMap<String, String>>,
}

#[cfg(test)]
impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl SecretBackend for MemoryBackend {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.values.lock().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.values.lock().insert(name.to_string(), value.to_string());
        Ok(())
    }
}

#[cfg(not(test))]
pub fn backend() -> &'static dyn SecretBackend {
    static B: KeyringBackend = KeyringBackend;
    &B
}

#[cfg(test)]
pub fn backend() -> &'static dyn SecretBackend {
    static B: once_cell::sync::Lazy<MemoryBackend> = once_cell::sync::Lazy::new(MemoryBackend::new);
    &*B
}
