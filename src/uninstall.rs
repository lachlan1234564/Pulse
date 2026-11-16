//! The "Uninstall Pulse" window. The uninstaller starts `pulse.exe --uninstall-options`
//! and waits: Pulse asks what else to delete from its data folder, in its own look (frosted
//! glass on Windows 11), and answers with its exit code, which the uninstaller acts on.
//! If this window can't open, the uninstaller asks with its own plain window instead.
//!
//! Exit code: 16 plus 1 (temporary files), 2 (downloaded songs) and 4 (library) for what to
//! delete. 16 alone keeps everything, which is also the answer when the window is closed.
//! Running the command by hand only shows the window: it never deletes anything itself.

use crate::theme::{heavy, med, pal, regular};
use egui::{Align2, Color32, CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

pub const ARG: &str = "--uninstall-options";

pub fn requested() -> bool {
    std::env::args().any(|a| a == ARG)
}

/// What each choice deletes, in Pulse's data folder (the uninstaller deletes the same).
const PARTS: [&[&str]; 3] = [
    &["cache", "tools", "logs", "crash-reports"],
    &["offline"],
    &["data", "covers", "themes", "backups", "library.ron", "account.json"],
];
const NAMES: [&str; 3] = ["Temporary files", "Downloaded songs", "Your library"];
const ABOUT: [&str; 3] = [
    "Cover art, downloaded tools and logs. Pulse makes these again when needed.",
    "Songs in Pulse's own downloads folder. A folder you picked yourself is kept.",
    "Likes, playlists, profile, themes, backups and settings. Keep it if you might come back.",
];

/// Shows the window and returns the exit code for the uninstaller.
pub fn run() -> i32 {
    let answer = Arc::new(Mutex::new(None::<u8>));
    let frosted = acrylic_supported();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Uninstall Pulse")
            .with_app_id("pulse-uninstall")
            .with_inner_size([460.0, 440.0])
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(frosted)
            .with_icon(crate::app_icon()),
        // OpenGL: the only renderer here that can show the frosted glass through the window.
        renderer: eframe::Renderer::Glow,
        centered: true,
        persist_window: false,
        // Nothing of this window is kept, and nothing goes in Pulse's data folder.
        persistence_path: Some(std::env::temp_dir().join("pulse-uninstall.ron")),
        ..Default::default()
    };
    let out = answer.clone();
    let result = eframe::run_native(
        "Pulse uninstall",
        options,
        Box::new(move |cc| {
            crate::theme::install(&cc.egui_ctx);
            let frosted = frosted && make_frosted(cc);
            Ok(Box::new(Window::new(out, frosted, &cc.egui_ctx)))
        }),
    );
    match result {
        Ok(()) => 16 + answer.lock().ok().and_then(|a| *a).unwrap_or(0) as i32,
        // The uninstaller asks with its own window instead.
        Err(_) => 1,
    }
}

struct Window {
    answer: Arc<Mutex<Option<u8>>>,
    frosted: bool,
    ticked: [bool; 3],
    sizes: Option<[u64; 3]>,
    sizes_rx: Option<Receiver<[u64; 3]>>,
}

impl Window {
    fn new(answer: Arc<Mutex<Option<u8>>>, frosted: bool, ctx: &egui::Context) -> Self {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let root = crate::settings::data_dir();
            let _ = tx.send(PARTS.map(|names| names.iter().map(|n| size_of(&root.join(n))).sum()));
            ctx.request_repaint();
        });
        Self { answer, frosted, ticked: [true, false, false], sizes: None, sizes_rx: Some(rx) }
    }

    fn finish(&self, ctx: &egui::Context, keep_all: bool) {
        let bits = if keep_all { 0 } else { self.ticked.iter().enumerate().map(|(i, &t)| (t as u8) << i).sum() };
        if let Ok(mut a) = self.answer.lock() {
            *a = Some(bits);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for Window {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.frosted { [0.0; 4] } else { Color32::from_rgb(0x12, 0x12, 0x12).to_normalized_gamma_f32() }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(rx) = &self.sizes_rx
            && let Ok(s) = rx.try_recv()
        {
            self.sizes = Some(s);
            self.sizes_rx = None;
        }
        let full = ui.max_rect();
        // A dark tint over the glass keeps the text readable on any wallpaper.
        let fill = if self.frosted { Color32::from_rgba_unmultiplied(14, 14, 16, 170) } else { Color32::from_rgb(0x12, 0x12, 0x12) };
        ui.painter().rect_filled(full, CornerRadius::ZERO, fill);

        // The top strip moves the window.
        let drag = ui.interact(Rect::from_min_size(full.min, vec2(full.width() - 48.0, 44.0)), egui::Id::new("drag"), Sense::drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        let close = Rect::from_center_size(pos2(full.right() - 26.0, full.top() + 24.0), Vec2::splat(30.0));
        let close_resp = ui.interact(close, egui::Id::new("close"), Sense::click()).on_hover_text("Keep everything");
        if close_resp.hovered() {
            ui.painter().rect_filled(close, CornerRadius::same(15), Color32::from_white_alpha(18));
        }
        ui.painter().text(close.center(), Align2::CENTER_CENTER, egui_phosphor::regular::X, regular(16.0), pal().text_muted);

        let inner = full.shrink2(vec2(28.0, 0.0));
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(inner.left(), full.top() + 30.0), pos2(inner.right(), full.bottom() - 24.0))));
        ui.label(RichText::new("Pulse is being removed.").font(heavy(22.0)).color(pal().text_muted));
        ui.label(RichText::new("Delete anything else?").font(heavy(22.0)).color(pal().text));
        ui.add_space(18.0);

        for i in 0..3 {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 74.0), Sense::click());
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                self.ticked[i] = !self.ticked[i];
            }
            let p = ui.painter();
            let card = if resp.hovered() { Color32::from_white_alpha(14) } else { Color32::from_white_alpha(8) };
            p.rect(rect, CornerRadius::same(16), card, Stroke::new(1.0, Color32::from_white_alpha(18)), StrokeKind::Inside);
            // A switch on the right.
            let on = self.ticked[i];
            let t = ui.ctx().animate_bool_with_time(resp.id, on, 0.15);
            let track = Rect::from_center_size(pos2(rect.right() - 34.0, rect.center().y), vec2(36.0, 20.0));
            let off_fill = Color32::from_white_alpha(40);
            p.rect_filled(track, CornerRadius::same(10), lerp(off_fill, pal().accent, t));
            p.circle_filled(pos2(track.left() + 10.0 + 16.0 * t, track.center().y), 7.0, Color32::WHITE);
            // Name, size and what it is.
            let left = rect.left() + 16.0;
            let size = match self.sizes {
                Some(s) => format!("  ·  {}", human_size(s[i])),
                None => String::new(),
            };
            let title = p.layout_no_wrap(format!("{}{size}", NAMES[i]), med(14.5), pal().text);
            p.galley(pos2(left, rect.top() + 14.0), title, pal().text);
            let about = p.layout(ABOUT[i].to_string(), regular(12.5), pal().text_muted, rect.width() - 84.0);
            p.galley(pos2(left, rect.top() + 36.0), about, pal().text_muted);
            if resp.has_focus() {
                p.rect_stroke(rect, CornerRadius::same(16), Stroke::new(2.0, pal().accent), StrokeKind::Outside);
            }
            ui.add_space(4.0);
        }

        // Buttons along the bottom: one primary.
        let bottom = ui.max_rect().bottom();
        let primary = Rect::from_min_size(pos2(inner.right() - 112.0, bottom - 36.0), vec2(112.0, 36.0));
        let resp = ui.interact(primary, egui::Id::new("continue"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        let fill = if resp.hovered() { pal().accent_soft } else { pal().accent };
        ui.painter().rect_filled(primary, CornerRadius::same(18), fill);
        ui.painter().text(primary.center(), Align2::CENTER_CENTER, "Continue", med(14.0), Color32::WHITE);
        let keep = ui.painter().layout_no_wrap("Keep everything".into(), med(14.0), pal().text_muted);
        let keep_rect = Rect::from_min_size(pos2(primary.left() - keep.size().x - 36.0, primary.top()), vec2(keep.size().x + 24.0, 36.0));
        let keep_resp = ui.interact(keep_rect, egui::Id::new("keep"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        if keep_resp.hovered() {
            ui.painter().rect_filled(keep_rect, CornerRadius::same(18), Color32::from_white_alpha(14));
        }
        ui.painter().galley(keep_rect.center() - keep.size() / 2.0, keep, pal().text_muted);

        let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if resp.clicked() || enter {
            self.finish(&ctx, false);
        } else if keep_resp.clicked() || close_resp.clicked() || esc {
            self.finish(&ctx, true);
        }
    }
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// Total size of a file or folder (0 if it isn't there).
fn size_of(path: &PathBuf) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).map(|d| d.flatten().map(|e| size_of(&e.path())).sum()).unwrap_or(0)
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0 => "empty".into(),
        b if b < 1 << 20 => format!("{} KB", b.div_ceil(1024)),
        b if b < 1 << 30 => format!("{:.0} MB", b as f64 / (1 << 20) as f64),
        b => format!("{:.1} GB", b as f64 / (1 << 30) as f64),
    }
}

/// Frosted glass ("acrylic") came with Windows 11 22H2 (build 22621).
#[cfg(windows)]
fn acrylic_supported() -> bool {
    #[repr(C)]
    struct OsVersion {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OsVersion) -> i32;
    }
    let mut v = OsVersion { size: std::mem::size_of::<OsVersion>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
    // SAFETY: a correctly sized struct with its size filled in, as RtlGetVersion expects.
    unsafe { RtlGetVersion(&mut v) == 0 && v.major >= 10 && v.build >= 22621 }
}
#[cfg(not(windows))]
fn acrylic_supported() -> bool {
    false
}

/// Asks Windows for a dark frosted-glass background and round corners. False if it refused
/// (the window is then drawn solid).
#[cfg(windows)]
fn make_frosted(cc: &eframe::CreationContext<'_>) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    }
    const USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const WINDOW_CORNER_PREFERENCE: u32 = 33;
    const SYSTEMBACKDROP_TYPE: u32 = 38;
    const ROUND: u32 = 2;
    const ACRYLIC: u32 = 3; // DWMSBT_TRANSIENTWINDOW
    let Ok(handle) = cc.window_handle() else { return false };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return false };
    let hwnd = h.hwnd.get();
    let set = |attr: u32, value: u32| {
        // SAFETY: a valid window and a u32 value of the size given, as these attributes take.
        unsafe { DwmSetWindowAttribute(hwnd, attr, (&value as *const u32).cast(), 4) == 0 }
    };
    set(USE_IMMERSIVE_DARK_MODE, 1);
    set(WINDOW_CORNER_PREFERENCE, ROUND);
    set(SYSTEMBACKDROP_TYPE, ACRYLIC)
}
#[cfg(not(windows))]
fn make_frosted(_cc: &eframe::CreationContext<'_>) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_well() {
        assert_eq!(human_size(0), "empty");
        assert_eq!(human_size(1500), "2 KB");
        assert_eq!(human_size(150 << 20), "150 MB");
        assert_eq!(human_size(3 << 29), "1.5 GB");
    }

    #[test]
    fn the_choices_match_the_uninstallers_folders() {
        // installer/pulse.iss deletes the same folders for each choice.
        let iss = include_str!("../installer/pulse.iss");
        for part in PARTS.iter().flat_map(|p| p.iter()) {
            assert!(iss.contains(&format!("'{part}'")), "{part} is missing from pulse.iss");
        }
    }
}
