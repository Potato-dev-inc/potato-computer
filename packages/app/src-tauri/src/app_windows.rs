//! The one window of the app.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const COCKPIT_LABEL: &str = "cockpit";
pub const COCKPIT_TITLE: &str = "My Computer";

pub fn open_cockpit_window(app: &AppHandle, focus: bool) {
    let app = app.clone();
    std::thread::spawn(move || {
        if let Some(window) = app.get_webview_window(COCKPIT_LABEL) {
            let _ = window.unminimize();
            let _ = window.show();
            if focus {
                let _ = window.set_focus();
            }
            return;
        }
        let built = WebviewWindowBuilder::new(&app, COCKPIT_LABEL, WebviewUrl::App("index.html".into()))
            .title(COCKPIT_TITLE)
            .inner_size(1100.0, 760.0)
            .min_inner_size(720.0, 480.0)
            .resizable(true)
            .focused(focus)
            .build();
        match built {
            Ok(window) => {
                let _ = window.show();
                if focus {
                    let _ = window.set_focus();
                }
                tracing::info!("My Computer window opened");
            }
            Err(e) => tracing::warn!(error = %e, "My Computer window failed to open"),
        }
    });
}

pub fn on_close_requested(app: &AppHandle, label: &str) -> bool {
    if label != COCKPIT_LABEL {
        return false;
    }
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.hide();
    }
    true
}
