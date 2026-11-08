mod app;
mod icons;
mod local_media;
mod native;
mod pages;
mod player;
mod plugins;
mod processing;
mod state;
mod updates;

pub fn launch() {
    use dioxus::desktop::{Config, WindowBuilder};
    let window = WindowBuilder::new()
        .with_title("YT Ultimate")
        .with_inner_size(dioxus::desktop::LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(dioxus::desktop::LogicalSize::new(720.0, 520.0));
    #[cfg(target_os = "macos")]
    let window = {
        use dioxus::desktop::tao::platform::macos::WindowBuilderExtMacOS;
        window
            .with_title_hidden(true)
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
    };
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            Config::new()
                .with_menu(native::menu())
                .with_custom_event_handler(|event, _| native::capture_open_events(event))
                .with_background_color((24, 24, 26, 255))
                .with_window(window)
                .with_icon(
                    dioxus::desktop::icon_from_memory(include_bytes!(
                        "../../../packaging/icons/icon.png"
                    ))
                    .expect("valid application icon"),
                ),
        )
        .launch(app::App);
}
