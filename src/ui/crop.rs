//! Cropping a picture before it becomes your profile picture (a circle) or a playlist
//! cover (a square). Drag to move the picture, scroll or use the slider to zoom. The
//! area outside the crop is dimmed, with an outline around what will be kept.
//!
//! The picture is read on a background thread; the result is a 512 × 512 PNG in the
//! temp folder, which the profile form or the playlist then copies like any picture.

use crate::app::App;
use crate::i18n::tr;
use crate::theme::*;
use egui::{Align, Color32, CornerRadius, Layout, Pos2, Rect, RichText, Sense, Stroke, Vec2, pos2, vec2};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// What the cropped picture is for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropFor {
    /// Your profile picture: kept round.
    Profile,
    /// The cover of your playlist at this position.
    #[cfg_attr(not(windows), allow(dead_code))]
    Playlist(usize),
}

impl CropFor {
    fn round(self) -> bool {
        self == CropFor::Profile
    }
}

/// An open crop window.
pub struct CropJob {
    pub target: CropFor,
    loading: Option<Receiver<Result<image::RgbaImage, String>>>,
    image: Option<image::RgbaImage>,
    texture: Option<egui::TextureHandle>,
    /// The picture's point at the middle of the crop (in its own pixels).
    center: Vec2,
    /// 1 = the picture's shorter side just fills the crop.
    zoom: f32,
    error: Option<String>,
}

impl CropJob {
    /// Starts reading `path` in the background.
    #[cfg_attr(not(windows), allow(dead_code))] // the picture picker is Windows-only
    pub fn open(path: PathBuf, target: CropFor) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = image::open(&path).map(|i| i.to_rgba8()).map_err(|e| format!("Couldn't read the picture ({e})"));
            let _ = tx.send(r);
        });
        Self { target, loading: Some(rx), image: None, texture: None, center: Vec2::ZERO, zoom: 1.0, error: None }
    }
}

/// The crop's size on screen, and the dimmed margin around it.
const CROP: f32 = 300.0;
const MARGIN: f32 = 22.0;
/// Most zoom allowed, and the saved picture's size.
const MAX_ZOOM: f32 = 5.0;
const OUT_SIDE: u32 = 512;

/// Pixels on screen per pixel of the picture.
fn scale_for(size: Vec2, zoom: f32) -> f32 {
    CROP / size.x.min(size.y) * zoom
}

/// Keeps the crop inside the picture: `center` moved as little as needed.
fn clamp_center(center: Vec2, size: Vec2, zoom: f32) -> Vec2 {
    let half = CROP / scale_for(size, zoom) / 2.0;
    vec2(center.x.clamp(half, (size.x - half).max(half)), center.y.clamp(half, (size.y - half).max(half)))
}

/// The part of the picture that's kept: (x, y, side) in its own pixels.
fn crop_box(center: Vec2, size: Vec2, zoom: f32) -> (u32, u32, u32) {
    let side = (CROP / scale_for(size, zoom)).min(size.x).min(size.y);
    let x = (center.x - side / 2.0).clamp(0.0, size.x - side);
    let y = (center.y - side / 2.0).clamp(0.0, size.y - side);
    (x.round() as u32, y.round() as u32, (side.round() as u32).max(1))
}

/// Cuts the kept square out and saves it as a 512 × 512 PNG in the temp folder.
fn save_crop(img: &image::RgbaImage, (x, y, side): (u32, u32, u32)) -> Result<PathBuf, String> {
    let side = side.min(img.width() - x).min(img.height() - y);
    let cut = image::imageops::crop_imm(img, x, y, side, side).to_image();
    let out = image::imageops::resize(&cut, OUT_SIDE, OUT_SIDE, image::imageops::FilterType::Triangle);
    let path = std::env::temp_dir().join(format!("pulse-crop-{}.png", crate::crash::now()));
    out.save(&path).map_err(|e| format!("Couldn't save the picture ({e})"))?;
    Ok(path)
}

/// Shows the crop window while one is open. "Use picture" hands the result on.
pub fn crop_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut job) = app.crop.take() else { return };
    if let Some(rx) = &job.loading
        && let Ok(r) = rx.try_recv()
    {
        job.loading = None;
        match r {
            Ok(img) => {
                // A small copy for the screen; the full picture is kept for the cut.
                let preview = image::imageops::thumbnail(&img, 1024.min(img.width()), (1024 * img.height() / img.width().max(1)).max(1).min(img.height()));
                let color = egui::ColorImage::from_rgba_unmultiplied([preview.width() as usize, preview.height() as usize], preview.as_raw());
                job.texture = Some(ctx.load_texture("crop-preview", color, egui::TextureOptions::LINEAR));
                job.center = vec2(img.width() as f32, img.height() as f32) / 2.0;
                job.image = Some(img);
            }
            Err(e) => job.error = Some(e),
        }
    }
    let (mut keep, mut apply) = (true, false);
    let round = job.target.round();
    egui::Modal::new(egui::Id::new("crop_picture")).frame(crate::ui::dialog_frame()).show(ctx, |ui| {
        let side = CROP + MARGIN * 2.0;
        ui.set_width(side);
        ui.label(RichText::new(if round { tr("Crop your picture") } else { tr("Crop the cover") }).font(heavy(20.0)).color(pal().text));
        ui.add_space(4.0);
        ui.label(RichText::new(tr("Drag to move it, scroll to zoom.")).font(regular(13.0)).color(pal().text_muted));
        ui.add_space(12.0);
        let (view, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click_and_drag());
        let painter = ui.painter_at(view);
        painter.rect_filled(view, CornerRadius::same(12), pal().surface_active);
        let crop = Rect::from_center_size(view.center(), Vec2::splat(CROP));
        match (&job.image, &job.texture) {
            (Some(img), Some(tex)) => {
                let size = vec2(img.width() as f32, img.height() as f32);
                if resp.dragged() {
                    job.center -= resp.drag_delta() / scale_for(size, job.zoom);
                }
                if resp.hovered() {
                    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        job.zoom = (job.zoom * (scroll * 0.004).exp()).clamp(1.0, MAX_ZOOM);
                    }
                }
                job.center = clamp_center(job.center, size, job.zoom);
                let scale = scale_for(size, job.zoom);
                let min: Pos2 = view.center() - job.center * scale;
                let shown = Rect::from_min_size(min, size * scale);
                painter.image(tex.id(), shown, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                // Dim everything outside the crop, then outline it.
                let dim = Color32::from_black_alpha(160);
                if round {
                    let wide = side;
                    painter.circle_stroke(crop.center(), CROP / 2.0 + wide / 2.0, Stroke::new(wide, dim));
                    painter.circle_stroke(crop.center(), CROP / 2.0, Stroke::new(2.0, Color32::WHITE));
                } else {
                    for r in [
                        Rect::from_min_max(view.min, pos2(view.max.x, crop.min.y)),
                        Rect::from_min_max(pos2(view.min.x, crop.max.y), view.max),
                        Rect::from_min_max(pos2(view.min.x, crop.min.y), pos2(crop.min.x, crop.max.y)),
                        Rect::from_min_max(pos2(crop.max.x, crop.min.y), pos2(view.max.x, crop.max.y)),
                    ] {
                        painter.rect_filled(r, CornerRadius::ZERO, dim);
                    }
                    painter.rect_stroke(crop, CornerRadius::same(6), Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Middle);
                }
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
                }
            }
            _ => {
                let text = job.error.clone().unwrap_or_else(|| tr("Opening the picture…").to_string());
                painter.text(view.center(), egui::Align2::CENTER_CENTER, text, regular(13.0), pal().text_muted);
                if job.error.is_none() {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
                }
            }
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(egui_phosphor::regular::MAGNIFYING_GLASS_MINUS).size(16.0).color(pal().text_muted));
            ui.spacing_mut().slider_width = side - 56.0;
            ui.add_enabled(job.image.is_some(), egui::Slider::new(&mut job.zoom, 1.0..=MAX_ZOOM).show_value(false))
                .on_hover_text(tr("Zoom"));
            ui.label(RichText::new(egui_phosphor::regular::MAGNIFYING_GLASS_PLUS).size(16.0).color(pal().text_muted));
        });
        ui.add_space(16.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ok = egui::Button::new(RichText::new(tr("Use picture")).font(med(14.0)).color(Color32::WHITE)).fill(pal().accent).corner_radius(16).min_size(vec2(0.0, 34.0));
            if ui.add_enabled(job.image.is_some(), ok).clicked() || (job.image.is_some() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                apply = true;
            }
            let cancel = egui::Button::new(RichText::new(tr("Cancel")).font(med(14.0))).frame_when_inactive(false).corner_radius(16).min_size(vec2(0.0, 34.0));
            if ui.add(cancel).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                keep = false;
            }
        });
    });
    if apply && let Some(img) = &job.image {
        let size = vec2(img.width() as f32, img.height() as f32);
        match save_crop(img, crop_box(job.center, size, job.zoom)) {
            Ok(path) => {
                app.picture_cropped(job.target, path, ctx);
                keep = false;
            }
            Err(e) => app.toast(&e, ctx),
        }
    }
    if keep {
        app.crop = Some(job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crop_stays_inside_the_picture() {
        let size = vec2(1600.0, 900.0);
        // Not zoomed: the crop is the picture's full height, a 900 × 900 square.
        let (x, y, side) = crop_box(clamp_center(vec2(0.0, 0.0), size, 1.0), size, 1.0);
        assert_eq!((x, y, side), (0, 0, 900), "dragged too far left: it stops at the edge");
        let (x, _, side) = crop_box(clamp_center(vec2(5000.0, 450.0), size, 1.0), size, 1.0);
        assert_eq!((x, side), (700, 900), "and at the right edge");
        // Zoomed in twice: a 450 square around the middle.
        let (x, y, side) = crop_box(clamp_center(size / 2.0, size, 2.0), size, 2.0);
        assert_eq!((x, y, side), (575, 225, 450));
    }

    #[test]
    fn saves_a_square_picture() {
        let img = image::RgbaImage::from_fn(300, 200, |x, _| if x < 150 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 255]) });
        let path = save_crop(&img, (0, 0, 200)).unwrap();
        let out = image::open(&path).unwrap().to_rgba8();
        assert_eq!((out.width(), out.height()), (OUT_SIDE, OUT_SIDE));
        assert_eq!(out.get_pixel(10, 10).0[0], 255, "the left part was kept");
        let _ = std::fs::remove_file(path);
    }
}
