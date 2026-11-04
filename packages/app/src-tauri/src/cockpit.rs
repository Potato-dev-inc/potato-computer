//! Opening the one window.

use tauri::AppHandle;

pub const NO_WINDOW_ENV: &str = "PUPPET_MASTER_NO_WINDOW";
pub const NO_WINDOW_ARG: &str = "--no-window";

pub fn startup_window(args: &[String], no_window_env: Option<&str>) -> bool {
    let env_off = no_window_env.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    !(env_off || args.iter().any(|a| a == NO_WINDOW_ARG))
}

pub fn open_primary_window(app: &AppHandle) {
    crate::app_windows::open_cockpit_window(app, true);
}
