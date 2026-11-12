mod activity;
mod activity_ui;
mod album_art;
mod album_ui;
mod app;
mod artist_online;
mod artist_ui;
mod audio_analysis;
mod autoplay;
mod bundled;
mod cards;
mod catalog;
mod concerts_ui;
mod context_menu;
mod credits_ui;
mod detail_ui;
mod discovery;
mod dj;
mod dj_analysis;
mod dj_analysis_ui;
mod dj_audio;
mod dj_ui;
mod downloads;
mod downloads_ui;
mod favorites;
mod favorites_ui;
mod following;
mod home_ui;
mod icons;
mod library_pages;
mod library_ui;
mod listening;
mod local_media;
mod lyrics;
mod lyrics_ui;
mod native;
mod native_instance;
mod navigation;
mod new_releases_ui;
mod now_playing;
mod pages;
mod player;
mod playlists_ui;
mod plugins;
mod processing;
mod processing_ui;
mod provider_cache;
mod queue_ui;
mod radio_ui;
mod region;
mod remote;
mod search;
mod search_results;
mod search_ui;
mod settings_ui;
mod state;
mod stations;
#[cfg(test)]
mod test_support;
mod theme;
mod updates;
mod video;

pub fn launch() {
    use dioxus::desktop::{Config, WindowBuilder};
    let _instance = match state::profile_path()
        .and_then(|profile| native_instance::acquire(&profile).map_err(|error| error.to_string()))
    {
        Ok(Some(guard)) => guard,
        Ok(None) => return,
        Err(error) => {
            rfd::MessageDialog::new()
                .set_title("No se pudo abrir YT Ultimate")
                .set_description(format!(
                    "No se pudo coordinar el acceso a la biblioteca: {error}"
                ))
                .set_level(rfd::MessageLevel::Error)
                .show();
            return;
        }
    };
    native::initialize();
    let config = Config::new();
    #[cfg(target_os = "macos")]
    let config = {
        // Tao must create its NSApplication subclass before menu construction
        // calls sharedApplication (Services and recent documents do so).
        // Otherwise AppKit retains a plain NSApplication for the whole process.
        config.with_event_loop(
            dioxus::desktop::tao::event_loop::EventLoopBuilder::with_user_event().build(),
        )
    };
    let window = WindowBuilder::new()
        .with_title("YT Ultimate")
        .with_inner_size(dioxus::desktop::LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(dioxus::desktop::LogicalSize::new(980.0, 660.0));
    #[cfg(target_os = "macos")]
    let window = {
        use dioxus::desktop::tao::platform::macos::WindowBuilderExtMacOS;
        window
            .with_transparent(true)
            .with_title_hidden(true)
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_traffic_light_inset(dioxus::desktop::LogicalPosition::new(20.0, 18.0))
    };
    #[cfg(target_os = "windows")]
    let window = {
        use dioxus::desktop::tao::platform::windows::WindowBuilderExtWindows;
        window.with_decorations(false).with_undecorated_shadow(true)
    };
    dioxus::LaunchBuilder::desktop()
        .with_cfg(native::configure(
            config
                .with_disable_context_menu(false)
                .with_menu(native::menu())
                .with_custom_event_handler(|event, _| native::capture_open_events(event))
                // Match the original BrowserWindow canvas beneath translucent CSS.
                .with_background_color((18, 18, 20, 255))
                .with_window(window)
                .with_icon(
                    dioxus::desktop::icon_from_memory(include_bytes!(
                        "../../../packaging/icons/icon.png"
                    ))
                    .expect("valid application icon"),
                ),
        ))
        .launch(app::App);
}
