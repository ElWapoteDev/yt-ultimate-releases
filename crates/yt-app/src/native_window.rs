//! Windows caption overlay; Mac keeps its native traffic lights.
use super::*;

#[component]
pub(crate) fn WindowControls() -> Element {
    #[cfg(not(target_os = "windows"))]
    {
        rsx! {}
    }
    #[cfg(target_os = "windows")]
    {
        let state = use_context::<AppState>();
        let window = use_window();
        let mut maximized = use_signal(|| window.window.is_maximized());
        let mut fullscreen = use_signal(|| window.window.fullscreen().is_some());
        let mut native_hover = use_signal(|| false);
        let event_window = window.clone();
        use_wry_event_handler(move |event, _| {
            if let Event::WindowEvent { window_id, .. } = event {
                if *window_id == event_window.id() {
                    let is_maximized = event_window.window.is_maximized();
                    let is_fullscreen = event_window.window.fullscreen().is_some();
                    if *maximized.peek() != is_maximized {
                        maximized.set(is_maximized);
                    }
                    if *fullscreen.peek() != is_fullscreen {
                        fullscreen.set(is_fullscreen);
                    }
                    frame::FULLSCREEN.store(is_fullscreen, std::sync::atomic::Ordering::Relaxed);
                    frame::SCALE.store(
                        event_window.window.scale_factor().to_bits(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
            }
        });
        use_hook(move || {
            spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(32)).await;
                    let hovered = frame::MAX_HOVER.load(std::sync::atomic::Ordering::Relaxed);
                    if *native_hover.peek() != hovered {
                        native_hover.set(hovered);
                    }
                }
            });
        });
        let minimize = window.clone();
        let maximize = window.clone();
        rsx! {
            style { {".ytu-window-controls{position:fixed;top:0;right:0;height:52px;width:138px;display:flex;z-index:1000000;background:#18181a;color:#f5f5f5;-webkit-app-region:no-drag}.ytu-caption-button{width:46px;height:52px;display:grid;place-items:center;color:inherit;border:0;background:transparent}.ytu-caption-button:hover,.ytu-caption-button.native-hover{background:rgba(255,255,255,.1)}.ytu-caption-close:hover{background:#c42b1c;color:white}.ytu-caption-button:active{background:rgba(255,255,255,.2)}.ytu-caption-close:active{background:#a52216}"} }
            if !*fullscreen.read() {
                div { class: "ytu-window-controls", onmousedown: |event| event.stop_propagation(),
                    button { r#type: "button", class: "ytu-caption-button", aria_label: "Minimizar", title: "Minimizar", onclick: move |_| minimize.window.set_minimized(true),
                        svg { width: 12, height: 12, view_box: "0 0 12 12", "aria-hidden": "true", path { d: "M1 6.5h10", stroke: "currentColor", stroke_width: "1" } }
                    }
                    button { r#type: "button", class: if *native_hover.read() { "ytu-caption-button native-hover" } else { "ytu-caption-button" }, aria_label: if *maximized.read() { "Restaurar" } else { "Maximizar" }, title: if *maximized.read() { "Restaurar" } else { "Maximizar" }, onclick: move |_| maximize.toggle_maximized(),
                        svg { width: 12, height: 12, view_box: "0 0 12 12", fill: "none", stroke: "currentColor", stroke_width: "1", "aria-hidden": "true",
                            if *maximized.read() { path { d: "M3.5 3.5v-2h7v7h-2M1.5 3.5h7v7h-7Z" } } else { path { d: "M1.5 1.5h9v9h-9Z" } }
                        }
                    }
                    button { r#type: "button", class: "ytu-caption-button ytu-caption-close", aria_label: "Cerrar ventana", title: "Cerrar", onclick: move |_| request_close(&state),
                        svg { width: 12, height: 12, view_box: "0 0 12 12", "aria-hidden": "true", path { d: "m1 1 10 10M11 1 1 11", stroke: "currentColor", stroke_width: "1" } }
                    }
                }
            }
        }
    }
}

pub(super) fn use_window_frame(state: &AppState) {
    #[cfg(target_os = "windows")]
    {
        let window = use_window();
        let profile = state.services.profile.clone();
        use_hook(move || {
            use dioxus::desktop::tao::platform::windows::WindowExtWindows;
            frame::SCALE.store(
                window.window.scale_factor().to_bits(),
                std::sync::atomic::Ordering::Relaxed,
            );
            if !frame::install(window.window.hwnd()) {
                record_log(&profile, "Could not install Windows caption hit testing");
            }
        });
    }
    #[cfg(not(target_os = "windows"))]
    let _ = state;
}

#[cfg(target_os = "windows")]
mod frame {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    pub(super) static MAX_HOVER: AtomicBool = AtomicBool::new(false);
    pub(super) static FULLSCREEN: AtomicBool = AtomicBool::new(false);
    pub(super) static SCALE: AtomicU64 = AtomicU64::new(1.0f64.to_bits());
    const SUBCLASS: usize = 0x595455;
    type Procedure = unsafe extern "system" fn(isize, u32, usize, isize, usize, usize) -> isize;
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct MouseTracking {
        size: u32,
        flags: u32,
        window: isize,
        hover_time: u32,
    }
    #[link(name = "comctl32")]
    unsafe extern "system" {
        fn SetWindowSubclass(
            window: isize,
            procedure: Option<Procedure>,
            id: usize,
            data: usize,
        ) -> i32;
        fn RemoveWindowSubclass(window: isize, procedure: Option<Procedure>, id: usize) -> i32;
        fn DefSubclassProc(window: isize, message: u32, wparam: usize, lparam: isize) -> isize;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn ScreenToClient(window: isize, point: *mut Point) -> i32;
        fn GetClientRect(window: isize, rect: *mut Rect) -> i32;
        fn TrackMouseEvent(event: *mut MouseTracking) -> i32;
        fn IsZoomed(window: isize) -> i32;
    }
    pub(super) fn install(window: isize) -> bool {
        // SAFETY: called on the UI thread with the live Tao window; no Rust
        // allocation is passed to the subclass. It removes itself on destroy.
        unsafe { SetWindowSubclass(window, Some(procedure), SUBCLASS, 0) != 0 }
    }
    unsafe extern "system" fn procedure(
        window: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        _id: usize,
        _data: usize,
    ) -> isize {
        // SAFETY: this is the registered Win32 subclass; message parameters are
        // passed unchanged when delegating to the original Tao window procedure.
        unsafe {
            if message == 0x0082 {
                // WM_NCDESTROY
                RemoveWindowSubclass(window, Some(procedure), SUBCLASS);
                MAX_HOVER.store(false, Ordering::Relaxed);
            }
            if message == 0x00a0 {
                // WM_NCMOUSEMOVE
                MAX_HOVER.store(wparam == 9, Ordering::Relaxed);
                let mut tracking = MouseTracking {
                    size: std::mem::size_of::<MouseTracking>() as u32,
                    flags: 0x12,
                    window,
                    hover_time: 0,
                };
                TrackMouseEvent(&mut tracking);
            } else if message == 0x02a2 {
                MAX_HOVER.store(false, Ordering::Relaxed);
            } // WM_NCMOUSELEAVE
            let result = DefSubclassProc(window, message, wparam, lparam);
            if message != 0x0084
                || FULLSCREEN.load(Ordering::Relaxed)
                || (result != 0 && result != 1)
            {
                return result;
            }
            let mut point = Point {
                x: (lparam as u16 as i16) as i32,
                y: ((lparam as usize >> 16) as u16 as i16) as i32,
            };
            let mut rect = Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if ScreenToClient(window, &mut point) == 0 || GetClientRect(window, &mut rect) == 0 {
                return result;
            }
            let scale = f64::from_bits(SCALE.load(Ordering::Relaxed));
            let x = point.x as f64;
            let y = point.y as f64;
            let width = rect.right as f64;
            let height = rect.bottom as f64;
            // Preserve resize borders when a custom frame covers the client area.
            let border = if IsZoomed(window) != 0 {
                0.0
            } else {
                6.0 * scale
            };
            let left = x < border;
            let right = x >= width - border;
            let top = y < border;
            let bottom = y >= height - border;
            if top && left {
                return 13;
            }
            if top && right {
                return 14;
            }
            if bottom && left {
                return 16;
            }
            if bottom && right {
                return 17;
            }
            if left {
                return 10;
            }
            if right {
                return 11;
            }
            if top {
                return 12;
            }
            if bottom {
                return 15;
            }
            // Microsoft requires HTMAXBUTTON to expose the Windows 11 Snap
            // Layout flyout for a custom maximize button. DefSubclassProc then
            // handles native non-client maximize/restore clicks itself.
            if y >= 0.0 && y < 52.0 * scale && x >= width - 92.0 * scale && x < width - 46.0 * scale
            {
                return 9;
            }
            result
        }
    }
}
