#![recursion_limit = "512"]

mod app_paths;
mod app_windows;
mod cockpit;
mod crash;
mod desk;
mod diagnostics;
mod flight;
mod keys;
mod p2p;
mod pairing_commands;
mod secrets;
mod tray;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,puppet_master_app_lib=debug")),
        )
        .with_target(false)
        .with_writer(flight::applog::Tee)
        .init();

    let mut context = tauri::generate_context!();
    if app_paths::profile().is_some() {
        context.config_mut().identifier = app_paths::app_id();
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            cockpit::open_primary_window(app);
        }))
        .invoke_handler(tauri::generate_handler![
            desk::commands::desk_snapshot,
            desk::commands::desk_command,
            desk::permissions::desk_permissions,
            desk::link::commands::desk_link_view,
            desk::link::commands::desk_link_set_side,
            desk::link::commands::desk_link_pause,
            desk::link::commands::desk_link_panic,
            desk::link::commands::desk_link_layout,
            desk::link::commands::desk_link_set_place,
            desk::link::prefs_commands::desk_link_set_keymap,
            desk::link::prefs_commands::desk_link_set_scroll,
            desk::link::prefs_commands::desk_link_set_pointer_speed,
            pairing_commands::pair_view,
            pairing_commands::pair_host_start,
            pairing_commands::pair_host_cancel,
            pairing_commands::pair_join,
            pairing_commands::pair_answer,
            pairing_commands::pair_nearby,
            pairing_commands::trust_set_permission,
            pairing_commands::trust_remove,
            diagnostics::app_log_lines,
        ])
        .setup(|app| {
            crash::install(app_paths::app_data_dir());

            let settings = serde_json::json!({ "desk_enabled": true });
            desk::apply_settings(&settings);
            pairing_commands::apply_settings(&settings);

            if let Err(e) = tray::init(app.handle()) {
                tracing::warn!(error = %e, "tray icon failed to start");
            }

            let args = std::env::args().collect::<Vec<_>>();
            let no_window_env = std::env::var(cockpit::NO_WINDOW_ENV).ok();
            if cockpit::startup_window(&args, no_window_env.as_deref()) {
                cockpit::open_primary_window(app.handle());
            }

            Ok(())
        })
        .build(context)
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = &event {
                crash::record_exit(&app_paths::app_data_dir(), *code, "ExitRequested");
                if code.is_none() {
                    api.prevent_exit();
                }
            }
            if let tauri::RunEvent::WindowEvent { label, event: tauri::WindowEvent::CloseRequested { api, .. }, .. } = &event {
                if app_windows::on_close_requested(app_handle, label) {
                    api.prevent_close();
                }
            }
            if matches!(event, tauri::RunEvent::Exit) {
                crash::record_exit(&app_paths::app_data_dir(), None, "Exit");
                desk::shutdown();
                pairing_commands::shutdown();
            }
        });
}
