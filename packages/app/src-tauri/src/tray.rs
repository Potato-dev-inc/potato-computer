//! Tray icon: open the window, quit, and show whether the mouse is shared.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;

const TRAY_ID: &str = "my-computer-tray";
const OPEN_ID: &str = "open";
const QUIT_ID: &str = "quit";

fn tooltip() -> String {
    if crate::desk::link::sharing_on() {
        "My Computer \u{b7} Mouse sharing is on".into()
    } else {
        "My Computer".into()
    }
}

pub fn init(app: &AppHandle) -> tauri::Result<()> {
    remember(app);
    let open = MenuItem::with_id(app, OPEN_ID, "Open My Computer", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, QUIT_ID, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &separator, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(tooltip())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            OPEN_ID => crate::cockpit::open_primary_window(app),
            QUIT_ID => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::cockpit::open_primary_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    tracing::info!("tray icon created");
    Ok(())
}

pub fn request_refresh() {
    if let Some(app) = APP.get() {
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_tooltip(Some(tooltip()));
        }
    }
}

static APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

pub fn remember(app: &AppHandle) {
    let _ = APP.set(app.clone());
}
