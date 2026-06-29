//! The **Render** dock tab and its cross-cut UI/app state.
//!
//! A deliberate still-image render is driven by the app's `RenderJob` (app.rs):
//! it pins the offscreen viewport to the chosen output resolution + render-quality
//! look and lets the temporal volumetric accumulation converge over several frames
//! (progressive refinement, like Blender's sample display), then reads the finished
//! plate back. This module owns:
//!   * [`RenderUiState`] — the shared mailbox: UI→app commands ([`RenderRequest`])
//!     and app→UI status/result ([`RenderStatus`] + the finished image/texture).
//!   * [`render_tab`] — the tab UI: a toolbar (Render/Cancel · Save), a progress
//!     strip (Sample i/N + elapsed), and the large converging preview.
//!   * [`save_image`] — the format-aware writer shared by the toolbar Save and the
//!     inspector's "Render to image file" auto-save.

use std::path::Path;

use egui::RichText;

use crate::scene::{RenderConfig, RenderFormat};
use crate::ui::theme;

/// Phase of the current (or last) render.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderPhase {
    /// No render in flight and none finished this session.
    #[default]
    Idle,
    /// Accumulating samples — progress is live.
    Rendering,
    /// Finished; the result is shown and can be saved.
    Complete,
}

/// Live progress the app publishes into the Ui each frame so the Render tab can
/// draw it (the tab has no access to the renderer/job directly).
#[derive(Clone, Default)]
pub struct RenderStatus {
    pub phase: RenderPhase,
    /// Accumulation passes completed.
    pub sample: u32,
    /// Target pass count.
    pub total: u32,
    /// Seconds since the job started.
    pub elapsed_s: f32,
    /// Output resolution of the in-flight/finished render.
    pub res: (u32, u32),
}

/// A command the UI raises this frame for the app loop to act on after the egui
/// pass (the inspector/tab can't reach the renderer, the dock, or the job).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RenderRequest {
    /// Start (or restart) a render with the current [`RenderConfig`].
    Start,
    /// Cancel the in-flight render.
    Cancel,
    /// Save the finished image — `copy` = "Save a Copy" (don't update the
    /// remembered output path).
    Save { copy: bool },
}

/// Cross-cut render state living on `Ui`: the UI↔app mailbox + the finished image.
#[derive(Default)]
pub struct RenderUiState {
    /// UI→app: a Start/Cancel/Save raised this frame (consumed by the app loop).
    pub request: Option<RenderRequest>,
    /// app→UI: live progress, republished every frame.
    pub status: RenderStatus,
    /// app→UI: the LIVE offscreen render-target texture, shown (with the tile
    /// reveal) while the render converges.
    pub preview_tex: Option<egui::TextureId>,
    /// app→UI: the finished image as an egui texture, for the preview.
    pub result_tex: Option<egui::TextureHandle>,
    /// app→UI: the finished image bytes, for Save-to-Disk.
    pub image: Option<image::RgbaImage>,
    /// app→UI: the GPU backend label currently in use (e.g. "Metal").
    pub gpu_active: String,
    /// app→UI: every available GPU backend label (the Backend dropdown options).
    pub gpu_available: Vec<String>,
    /// UI→app: the backend the user picked (applied on the next launch). The app
    /// persists it when it differs from what was last written.
    pub gpu_selected: String,
}

impl RenderUiState {
    /// Raise a request, but only if one isn't already pending this frame — so two
    /// controls firing in the same egui pass (e.g. a Save click + an F12 press)
    /// can't clobber each other; the first one wins and the app consumes it.
    pub fn set_request(&mut self, req: RenderRequest) {
        if self.request.is_none() {
            self.request = Some(req);
        }
    }
    pub fn request_start(&mut self) {
        self.set_request(RenderRequest::Start);
    }
    pub fn request_cancel(&mut self) {
        self.set_request(RenderRequest::Cancel);
    }
}

/// The **Render** dock tab: a toolbar, a progress strip, and the large preview of
/// the converging (then final) render. While [`RenderPhase::Rendering`] it shows
/// the live offscreen render target ([`RenderUiState::preview_tex`]) with a
/// Blender-style tile reveal; once complete it shows the stable
/// [`RenderUiState::result_tex`].
pub fn render_tab(ui: &mut egui::Ui, config: &RenderConfig, state: &mut RenderUiState) {
    use theme::icon;
    let pal = theme::Palette::get(ui);

    // Snapshot the immutable bits up front so the &mut borrow for `request` below
    // never overlaps the reads (TextureHandle is a cheap Arc clone).
    let status = state.status.clone();
    let has_image = state.image.is_some();
    let result_tex = state.result_tex.clone();
    let rendering = status.phase == RenderPhase::Rendering;
    let complete = status.phase == RenderPhase::Complete;

    // Keep the frame loop alive while rendering so the timer + preview animate.
    if rendering {
        ui.ctx().request_repaint();
    }

    // --- Toolbar ------------------------------------------------------------
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        if rendering {
            if ui
                .add(egui::Button::new(
                    RichText::new(format!("{}  Cancel", icon::RENDER_STOP)).color(pal.conflict),
                ))
                .on_hover_text("Stop the render")
                .clicked()
            {
                state.request_cancel();
            }
        } else {
            let label = if complete || has_image {
                "Re-render"
            } else {
                "Render"
            };
            if ui
                .add(egui::Button::new(format!("{}  {label}", icon::RENDER_GO)))
                .on_hover_text("Render the current view with the World ▸ Render settings")
                .clicked()
            {
                state.request_start();
            }
        }
        ui.separator();
        ui.add_enabled_ui(has_image, |ui| {
            if ui
                .button(format!("{}  Save to Disk", icon::SAVE_IMAGE))
                .on_hover_text("Save the rendered image to a file")
                .clicked()
            {
                state.set_request(RenderRequest::Save { copy: false });
            }
            if ui
                .button(format!("{}  Save Copy", icon::EXPORT))
                .on_hover_text("Save a copy without changing the remembered output path")
                .clicked()
            {
                state.set_request(RenderRequest::Save { copy: true });
            }
        });

        // Right-aligned resolution/format readout.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (w, h) = if status.res != (0, 0) {
                status.res
            } else {
                config.output_size()
            };
            ui.label(
                RichText::new(format!(
                    "{w}×{h} · {} · {}%",
                    config.format.label(),
                    config.resolution_percentage
                ))
                .monospace()
                .weak(),
            );
        });
    });
    ui.separator();

    // --- Progress strip -----------------------------------------------------
    if rendering || complete {
        let frac = if status.total > 0 {
            (status.sample as f32 / status.total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mins = (status.elapsed_s as u32) / 60;
        let secs = (status.elapsed_s as u32) % 60;
        let txt = if complete {
            format!(
                "Done · {}/{} · {:02}:{:02}",
                status.sample, status.total, mins, secs
            )
        } else {
            format!(
                "Sample {}/{} · {:02}:{:02}",
                status.sample, status.total, mins, secs
            )
        };
        ui.horizontal(|ui| {
            let bar_w = (ui.available_width() - 170.0).max(60.0);
            ui.add(
                egui::ProgressBar::new(frac)
                    .desired_width(bar_w)
                    .desired_height(10.0)
                    .fill(if complete { pal.ok } else { pal.accent }),
            );
            ui.label(RichText::new(txt).monospace());
        });
        ui.add_space(3.0);
    }

    // --- Preview ------------------------------------------------------------
    let avail = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, pal.canvas);

    // Output resolution (for the aspect).
    let (rw, rh) = if status.res != (0, 0) {
        status.res
    } else {
        config.output_size()
    };
    let aspect = rw as f32 / rh.max(1) as f32;

    // Pick the source: the stable finished texture when not actively rendering,
    // else the LIVE offscreen render target (separate from the viewport).
    let preview: Option<egui::TextureId> =
        if let Some(tex) = result_tex.as_ref().filter(|_| !rendering) {
            Some(tex.id())
        } else if rendering {
            state.preview_tex
        } else {
            None
        };

    match preview {
        Some(tex_id) => {
            // EEVEE-style WHOLE-IMAGE progressive display: our renderer is a raster +
            // raymarch engine that renders the entire frame each pass and accumulates
            // a temporal EMA — like EEVEE's TAA, not Cycles' buckets — so the whole
            // image is shown from the first sample and simply refines as it converges
            // (the progress strip above reports Sample i/N).
            let fit = fit_rect(rect, aspect);
            ui.painter().image(
                tex_id,
                fit,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            // Idle, nothing rendered yet — a muted placeholder.
            let center = rect.center();
            let p = ui.painter();
            p.text(
                center - egui::vec2(0.0, 18.0),
                egui::Align2::CENTER_CENTER,
                icon::RENDER,
                egui::FontId::proportional(40.0),
                pal.ink_muted,
            );
            p.text(
                center + egui::vec2(0.0, 14.0),
                egui::Align2::CENTER_CENTER,
                "No render yet",
                egui::FontId::proportional(15.0),
                pal.ink_tertiary,
            );
            p.text(
                center + egui::vec2(0.0, 36.0),
                egui::Align2::CENTER_CENTER,
                // The bundled fonts have no "▸" glyph (renders as a tofu box) — use the
                // Phosphor caret, which IS in the merged icon font.
                format!(
                    "Press Render here or in World {}  Render Properties",
                    icon::NEXT
                ),
                egui::FontId::proportional(12.0),
                pal.ink_muted,
            );
        }
    }
}

/// Compute the largest rect of the given `aspect` (w/h) that fits inside `outer`,
/// centred — letterboxing the preview so the render isn't stretched.
fn fit_rect(outer: egui::Rect, aspect: f32) -> egui::Rect {
    let ow = outer.width();
    let oh = outer.height();
    if ow <= 0.0 || oh <= 0.0 || aspect <= 0.0 {
        return outer;
    }
    let outer_aspect = ow / oh;
    let (w, h) = if outer_aspect > aspect {
        (oh * aspect, oh) // height-limited
    } else {
        (ow, ow / aspect) // width-limited
    };
    egui::Rect::from_center_size(outer.center(), egui::vec2(w, h))
}

/// A sensible default output filename for the Save dialog, derived from the
/// remembered output path (if any) or a generic name + the format extension.
pub fn default_filename(config: &RenderConfig) -> String {
    let stem = Path::new(&config.out_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "render".to_string());
    format!("{stem}.{}", config.format.ext())
}

/// Write an RGBA8 render to disk in the chosen format. JPEG drops alpha (no
/// alpha support); EXR promotes the tonemapped 8-bit plate to float so the file
/// is a valid OpenEXR (the data is still LDR — we read back the post-tonemap
/// target).
pub fn save_image(
    img: &image::RgbaImage,
    path: &Path,
    fmt: RenderFormat,
) -> image::ImageResult<()> {
    match fmt {
        RenderFormat::Png => img.save_with_format(path, image::ImageFormat::Png),
        RenderFormat::Jpeg => image::DynamicImage::ImageRgba8(img.clone())
            .to_rgb8()
            .save_with_format(path, image::ImageFormat::Jpeg),
        RenderFormat::Exr => {
            let buf = image::Rgba32FImage::from_fn(img.width(), img.height(), |x, y| {
                let p = img.get_pixel(x, y).0;
                image::Rgba([
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                    p[3] as f32 / 255.0,
                ])
            });
            buf.save_with_format(path, image::ImageFormat::OpenExr)
        }
    }
}
