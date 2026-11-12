#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = yt_updater::handle_update_arguments() {
        std::process::exit(code);
    }
    yt_app::launch();
}
