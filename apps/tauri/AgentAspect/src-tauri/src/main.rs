// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod binary_locator;
mod bridge;
mod paths;

use std::path::PathBuf;
use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::Manager;

/// Tauri state holding the resource dir for binary discovery.
struct AppState {
    resource_dir: PathBuf,
}

#[tauri::command]
fn check_bridge_status(state: tauri::State<AppState>) -> bridge::BridgeStatus {
    bridge::status(Some(&state.resource_dir))
}

#[tauri::command]
fn start_bridge(state: tauri::State<AppState>) -> bridge::BridgeStatus {
    bridge::start(Some(&state.resource_dir))
}

#[tauri::command]
fn stop_bridge(state: tauri::State<AppState>) -> String {
    bridge::stop(Some(&state.resource_dir))
}

#[tauri::command]
fn get_bridge_url() -> Option<String> {
    bridge::bridge_url()
}

#[tauri::command]
fn toggle_theme(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // The bridge frontend exposes toggleTheme() in the global scope via view_model.js
        let _ = window.eval("toggleTheme()");
    }
}

#[tauri::command]
fn get_diagnostics(state: tauri::State<AppState>) -> serde_json::Value {
    let status = bridge::status(Some(&state.resource_dir));
    let binary_path = binary_locator::locate_binary(Some(&state.resource_dir));
    serde_json::json!({
        "data_dir": paths::data_dir().to_string_lossy(),
        "binary_path": binary_path.map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| "(not found)".into()),
        "bridge_status": status.display_summary,
        "pid": status.pid,
        "addr": status.addr,
        "lan_enabled": status.lan_enabled,
        "launchd_loaded": status.launchd_loaded,
        "keep_awake": status.keep_awake,
        "token_path": paths::bridge_token_path().to_string_lossy(),
        "log_file": paths::daemon_log_path().to_string_lossy(),
        "audit_db": paths::audit_db_path().to_string_lossy(),
    })
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let resource_dir = app
                .path()
                .resource_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            app.manage(AppState { resource_dir });

            // Build a View menu with Toggle Theme (Cmd+Shift+T)
            let toggle_theme_item = MenuItemBuilder::new("Toggle Theme")
                .id("toggle_theme")
                .accelerator("CmdOrCtrl+Shift+T")
                .build(app)?;
            let view_menu = SubmenuBuilder::new(app, "View")
                .item(&toggle_theme_item)
                .build()?;
            let menu = MenuBuilder::new(app).item(&view_menu).build()?;
            app.set_menu(menu)?;

            // Handle menu events
            let app_handle = app.handle().clone();
            app.on_menu_event(move |_app, event| {
                if event.id() == "toggle_theme" {
                    if let Some(window) = app_handle.get_webview_window("main") {
                        let _ = window.eval("toggleTheme()");
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_bridge_status,
            start_bridge,
            stop_bridge,
            get_bridge_url,
            toggle_theme,
            get_diagnostics,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
