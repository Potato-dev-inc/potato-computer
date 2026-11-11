//! Stable application data paths.

use std::path::PathBuf;

const APP_ID: &str = "com.potatodev.computer";
pub const PROFILE_ENV: &str = "PUPPET_MASTER_PROFILE";

pub fn profile_from(raw: Option<&str>) -> Option<String> {
    let name = raw?.trim().to_ascii_lowercase();
    let valid = !name.is_empty()
        && name.len() <= 32
        && name != "default"
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    valid.then_some(name)
}

pub fn profile() -> Option<String> {
    static PROFILE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    PROFILE
        .get_or_init(|| profile_from(std::env::var(PROFILE_ENV).ok().as_deref()))
        .clone()
}

pub fn app_id_for(profile: Option<&str>) -> String {
    match profile {
        None => APP_ID.to_string(),
        Some(name) => format!("{APP_ID}.profile-{name}"),
    }
}

pub fn app_id() -> String {
    app_id_for(profile().as_deref())
}

fn app_data_root() -> PathBuf {
    let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
    #[cfg(target_os = "macos")]
    {
        return home.join("Library").join("Application Support");
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or(home);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".local").join("share")
    }
}

pub fn app_data_dir() -> PathBuf {
    app_data_root().join(app_id())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}
