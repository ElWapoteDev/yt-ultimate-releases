use dioxus::prelude::*;

/// Lucide-style, stroke-only icons rendered from Rust components.
#[component]
pub fn Icon(name: String, #[props(default = 16)] size: u32) -> Element {
    let paths: &[&str] = match name.as_str() {
        "home" => &["m3 10 9-7 9 7v10a1 1 0 0 1-1 1h-6v-7H10v7H4a1 1 0 0 1-1-1Z"],
        "search" => &["m21 21-4.4-4.4", "M19 10.5a8.5 8.5 0 1 1-17 0 8.5 8.5 0 0 1 17 0"],
        "music" => &["M9 18V5l12-2v13", "M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0", "M21 16a3 3 0 1 1-6 0 3 3 0 0 1 6 0"],
        "playlist" => &["M4 6h16M4 11h10M4 16h7M18 16V9l4-1", "M18 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0"],
        "download" => &["M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"],
        "settings" => &["m9 3-1 3-3 1-2 3 2 2-1 3 2 3 3-1 2 3h3l1-3 3-1 2-3-2-2 1-3-2-3-3 1-2-3Z", "M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0"],
        "play" => &["m7 3 14 9-14 9Z"],
        "pause" => &["M7 4v16M17 4v16"],
        "next" => &["m4 4 12 8-12 8ZM20 4v16"],
        "previous" => &["m20 4-12 8 12 8ZM4 4v16"],
        "shuffle" => &["m18 14 4 4-4 4M18 2l4 4-4 4M2 18h3c5 0 7-12 12-12h5M2 6h3c2 0 3 2 4 4M15 16c1 2 2 2 7 2"],
        "repeat" => &["m17 2 4 4-4 4M3 11V8a2 2 0 0 1 2-2h16M7 22l-4-4 4-4M21 13v3a2 2 0 0 1-2 2H3"],
        "volume" => &["m11 5-6 4H2v6h3l6 4ZM15 8a6 6 0 0 1 0 8M18 5a10 10 0 0 1 0 14"],
        "plus" => &["M12 5v14M5 12h14"],
        "close" => &["m6 6 12 12M18 6 6 18"],
        "more" => &["M4 12h.01M12 12h.01M20 12h.01"],
        "heart" => &["M20.8 4.6a5.5 5.5 0 0 0-7.8 0L12 5.7l-1.1-1.1a5.5 5.5 0 0 0-7.8 7.8L12 21l8.8-8.6a5.5 5.5 0 0 0 0-7.8Z"],
        "check" => &["m5 12 4 4L19 6"],
        "trash" => &["M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7M14 10v7"],
        "folder" => &["M3 7V4h6l2 3h10v13H3ZM3 11h18"],
        "grid" => &["M3 3h7v7H3ZM14 3h7v7h-7ZM3 14h7v7H3ZM14 14h7v7h-7Z"],
        "list" => &["M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"],
        "arrow" => &["M5 12h14m-5-5 5 5-5 5"],
        "video" => &["M3 5h13v14H3ZM16 10l6-4v12l-6-4"],
        "scissors" => &["m8 8 13 13M14 10l7-7M8 16l3-3", "M9 6a3 3 0 1 1-6 0 3 3 0 0 1 6 0", "M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0"],
        _ => &["M12 3v10M12 18v1"],
    };
    rsx! {
        svg { width: size, height: size, view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.8", stroke_linecap: "round", stroke_linejoin: "round", "aria-hidden": "true",
            for path in paths { path { d: *path } }
        }
    }
}
