#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod models;
mod svn;

use commands::*;

fn main() {
    tracing_subscriber::fmt::init();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![
            configure_svn_executable,
            launch_workspace_path,
            svn_path_is_working_copy,
            svn_checkout_stream,
            svn_checkout_salvage,
            svn_resume_checkout,
            svn_cancel,
            svn_repair_names,
            svn_delete_duplicates,
            svn_update,
            svn_commit,
            svn_status,
            svn_log,
            svn_current_user,
            svn_info,
            svn_local_revision,
            svn_remote_info,
            svn_diff,
            svn_apply_patch,
            svn_blame,
            svn_add,
            svn_delete,
            svn_revert,
            svn_resolve,
            svn_cleanup,
            svn_switch,
            svn_merge,
            open_workspace_target,
            reveal_workspace_file,
            delete_unversioned,
            read_unversioned_files,
            restore_unversioned_files,
            read_gitignore,
            save_stash,
            list_stashes,
            delete_stash,
            import_legacy_stashes,
        ]);

    #[cfg(target_os = "macos")]
    let builder = builder.on_window_event(|window, event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = window.hide();
        }
    });

    builder
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            handle_macos_event(_app, _event);
        });
}

/// Cmd+W belongs to the window, Cmd+Q to the app.
///
/// Closing the window is intercepted on macOS so it hides instead of destroying
/// the last window: destroying it makes the runtime exit the whole process, which
/// looked like "Cmd+W quits the app". Hiding keeps the session in the Dock, and
/// clicking the Dock icon brings the window back. Cmd+Q still terminates the app
/// through `NSApplication`, which is a different path and is untouched here.
#[cfg(target_os = "macos")]
fn handle_macos_event(app: &tauri::AppHandle, event: tauri::RunEvent) {
    use tauri::Manager;

    if let tauri::RunEvent::Reopen { .. } = event {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}
