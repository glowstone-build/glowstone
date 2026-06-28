//! LED-screen inspector editor. Pure code move out of [`super`] (the inspector
//! `mod.rs`): the editor shown when an LED screen is selected.

use super::theme;
use super::{category, row, slider_row, vec3_rows, InspectorState};
use super::ScreenSources;
use crate::scene::screen::{LedScreen, PixelShape, ScreenContent, TestPattern};
use egui::{DragValue, Grid, RichText, Slider};
use glam::{Mat4, Vec3};

/// Inspector for a selected LED screen: identity, transform, the parametric
/// cabinet grid (with a live derived-resolution readout), surface photometry,
/// and the content source. Phase 1 covers Test Pattern + Solid Colour content;
/// the cabinet is editable directly (the panel TYPE is set from the Library).
pub(super) fn led_screen_inspector(
    ui: &mut egui::Ui,
    s: &mut LedScreen,
    count: usize,
    sources: &ScreenSources,
    state: &mut InspectorState,
    show_transform: bool,
) {
    ui.heading(s.name.as_str());
    let [rx, ry] = s.resolution();
    let [mw, mh] = s.size_m();
    ui.label(
        RichText::new(format!("{} · {} × {} px · {:.2} × {:.2} m", s.panel_type, rx, ry, mw, mh))
            .weak()
            .small(),
    );
    if count > 1 {
        ui.label(
            RichText::new(format!("{count} screens — transform applies to all; other edits affect the active screen"))
                .weak()
                .small(),
        );
    }
    ui.separator();

    ui.horizontal(|ui| {
        let mut visible = !s.hidden;
        if ui.checkbox(&mut visible, "Visible").changed() {
            s.hidden = !visible;
        }
        ui.separator();
        ui.label(RichText::new("Seq").weak().small());
        ui.add(DragValue::new(&mut s.sequence).range(1..=u32::MAX).speed(0.2));
    });

    if show_transform {
        // --- Transform (position / rotation / uniform scale, lossless like geometry) ---
        let (scale0, rot0, _t0) = s.transform.to_scale_rotation_translation();
        let mut pos = s.transform.w_axis.truncate();
        let mut uscale = ((scale0.x + scale0.y + scale0.z) / 3.0).max(1e-3);
        let (ryr, rxr, rzr) = rot0.to_euler(glam::EulerRot::YXZ);
        let (mut ey, mut ex, mut ez) = (ryr.to_degrees(), rxr.to_degrees(), rzr.to_degrees());
        let mut pos_changed = false;
        let mut rs_changed = false;
        category(
            ui,
            state,
            "Transform",
            format!("{}  Transform", theme::icon::INSPECTOR),
            true,
            &["Position", "Rotation", "Scale"],
            |ui, fs| {
                Grid::new("led-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                    // Position/Rotation STACK X/Y/Z (Blender-style) — readable at any width.
                    pos_changed |= vec3_rows(
                        ui, fs, "Position", false, 0.05, "",
                        &mut pos.x, &mut pos.y, &mut pos.z,
                    );
                    rs_changed |= vec3_rows(ui, fs, "Rotation", false, 0.5, "°", &mut ex, &mut ey, &mut ez);
                    row(ui, fs, "Scale", false, |ui| {
                        rs_changed |= ui.add(DragValue::new(&mut uscale).speed(0.005).range(0.001..=1000.0)).changed();
                    });
                });
            },
        );
        if rs_changed {
            let rot = glam::Quat::from_euler(glam::EulerRot::YXZ, ey.to_radians(), ex.to_radians(), ez.to_radians());
            s.transform = Mat4::from_scale_rotation_translation(Vec3::splat(uscale), rot, pos);
        } else if pos_changed {
            s.transform.w_axis = pos.extend(1.0);
        }
    }

    // --- Panel: one cabinet's size + native pixels (pitch is derived) ---
    category(
        ui,
        state,
        "Panel",
        format!("{}  Panel", theme::icon::SCREEN),
        true,
        &["Cabinet (mm)", "Pixels / cabinet", "Pitch"],
        |ui, fs| {
            Grid::new("led-panel").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                row(ui, fs, "Cabinet (mm)", false, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(DragValue::new(&mut s.cabinet_mm[0]).speed(1.0).range(50.0..=2000.0).prefix("w "));
                        ui.add(DragValue::new(&mut s.cabinet_mm[1]).speed(1.0).range(50.0..=2000.0).prefix("h "));
                    });
                });
                row(ui, fs, "Pixels / cabinet", false, |ui| {
                    ui.horizontal(|ui| {
                        let mut px = s.cabinet_px[0] as i32;
                        let mut py = s.cabinet_px[1] as i32;
                        if ui.add(DragValue::new(&mut px).speed(1.0).range(8..=1024).prefix("x ")).changed() {
                            s.cabinet_px[0] = px.max(1) as u32;
                        }
                        if ui.add(DragValue::new(&mut py).speed(1.0).range(8..=1024).prefix("y ")).changed() {
                            s.cabinet_px[1] = py.max(1) as u32;
                        }
                    });
                });
                row(ui, fs, "Pitch", false, |ui| {
                    ui.label(RichText::new(format!("{:.2} mm", s.pitch_mm())).weak());
                });
            });
        },
    );

    // --- Array: panels wide × high → live derived total resolution + size ---
    category(
        ui,
        state,
        "Array",
        format!("{}  Array", theme::icon::PATCH),
        true,
        &["Panels", "Gap"],
        |ui, fs| {
            Grid::new("led-array").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                row(ui, fs, "Panels", false, |ui| {
                    ui.horizontal(|ui| {
                        let mut w = s.panels_wide as i32;
                        let mut h = s.panels_high as i32;
                        if ui.add(DragValue::new(&mut w).speed(0.1).range(1..=64).prefix("w ")).changed() {
                            s.panels_wide = w.max(1) as u32;
                        }
                        if ui.add(DragValue::new(&mut h).speed(0.1).range(1..=64).prefix("h ")).changed() {
                            s.panels_high = h.max(1) as u32;
                        }
                    });
                });
                row(ui, fs, "Gap", false, |ui| {
                    ui.add(DragValue::new(&mut s.gap_mm).speed(0.1).range(0.0..=50.0).suffix(" mm"));
                });
            });
            let [rx, ry] = s.resolution();
            let [mw, mh] = s.size_m();
            let mpx = (rx as f64 * ry as f64) / 1_000_000.0;
            ui.label(
                RichText::new(format!("{rx} × {ry} px  ·  {mpx:.2} Mpx  ·  {mw:.2} × {mh:.2} m"))
                    .strong()
                    .small(),
            );
        },
    );

    // --- Surface: photometry + transparency + curvature ---
    category(
        ui,
        state,
        "Surface",
        format!("{}  Surface", theme::icon::COLOR),
        true,
        &["Brightness", "Light emit", "Gamma", "Transparency", "Curvature", "Pixel"],
        |ui, fs| {
            Grid::new("led-surface").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                row(ui, fs, "Brightness", false, |ui| {
                    ui.add(DragValue::new(&mut s.nits).speed(10.0).range(50.0..=8000.0).suffix(" nits"));
                });
                row(ui, fs, "Light emit", false, |ui| {
                    ui.add(DragValue::new(&mut s.emit).speed(0.02).range(0.0..=4.0))
                        .on_hover_text("How much the wall lights the scene + haze (0 = none)");
                });
                row(ui, fs, "Gamma", false, |ui| {
                    ui.add(DragValue::new(&mut s.gamma).speed(0.01).range(1.0..=3.0));
                });
                slider_row(ui, fs, "Transparency", false, |ui| {
                    let mut transp = 1.0 - s.opacity;
                    if ui.add(Slider::new(&mut transp, 0.0..=1.0)).on_hover_text("See-through / mesh LED").changed() {
                        s.opacity = (1.0 - transp).clamp(0.0, 1.0);
                    }
                });
                row(ui, fs, "Curvature", false, |ui| {
                    ui.add(DragValue::new(&mut s.curvature_deg).speed(0.5).range(-60.0..=60.0).suffix("°"))
                        .on_hover_text("Horizontal arc subtended across the wall");
                });
                row(ui, fs, "Pixel", false, |ui| {
                    egui::ComboBox::from_id_salt("led-pixel-shape")
                        .selected_text(s.pixel_shape.label())
                        .show_ui(ui, |ui| {
                            for sh in PixelShape::ALL {
                                ui.selectable_value(&mut s.pixel_shape, sh, sh.label());
                            }
                        })
                        .response
                        .on_hover_text("LED package shape seen up close (SMD round/square, or discrete RGB sub-pixels)");
                });
            });
        },
    );

    // --- Content: the source shown on the surface ---
    // Whole-category filter only (the body is combo/dynamic, not grid rows).
    category(
        ui,
        state,
        "Content",
        format!("{}  Content", theme::icon::IMAGE),
        true,
        &["Content", "Source", "Pattern", "Colour", "Image", "Grid", "Patch"],
        |ui, _fs| {
            #[derive(PartialEq, Clone, Copy)]
            enum Kind {
                Test,
                Solid,
                Image,
                Ndi,
                Citp,
                Dmx,
            }
            let cur = match &s.content {
                ScreenContent::TestPattern(_) => Kind::Test,
                ScreenContent::SolidColor(_) => Kind::Solid,
                ScreenContent::Image { .. } => Kind::Image,
                ScreenContent::Ndi { .. } => Kind::Ndi,
                ScreenContent::Citp { .. } => Kind::Citp,
                ScreenContent::PixelMapDmx(_) => Kind::Dmx,
            };
            let mut sel = cur;
            ui.horizontal(|ui| {
                ui.label("Source");
                egui::ComboBox::from_id_salt("led-source").selected_text(s.content.label()).show_ui(ui, |ui| {
                    ui.selectable_value(&mut sel, Kind::Test, "Test Pattern");
                    ui.selectable_value(&mut sel, Kind::Solid, "Solid Colour");
                    ui.selectable_value(&mut sel, Kind::Image, "Image…");
                    ui.selectable_value(&mut sel, Kind::Ndi, "NDI");
                    ui.selectable_value(&mut sel, Kind::Citp, "CITP");
                    ui.selectable_value(&mut sel, Kind::Dmx, "Pixel-map DMX");
                });
            });
            if sel != cur {
                s.frame = None; // drop any live frame from the previous source
                s.content = match sel {
                    Kind::Test => ScreenContent::TestPattern(TestPattern::Grid),
                    Kind::Solid => ScreenContent::SolidColor([0.1, 0.4, 0.9]),
                    Kind::Image => {
                        ScreenContent::Image { name: String::new(), bytes: std::sync::Arc::new(Vec::new()) }
                    }
                    Kind::Ndi => ScreenContent::Ndi { source: String::new() },
                    Kind::Citp => ScreenContent::Citp { source: String::new() },
                    Kind::Dmx => ScreenContent::PixelMapDmx(crate::scene::screen::PixelMap::default()),
                };
            }
            ui.add_space(2.0);
            match &mut s.content {
                ScreenContent::TestPattern(tp) => {
                    ui.horizontal(|ui| {
                        ui.label("Pattern");
                        for p in TestPattern::ALL {
                            ui.selectable_value(tp, p, p.label());
                        }
                    });
                }
                ScreenContent::SolidColor(c) => {
                    ui.horizontal(|ui| {
                        ui.label("Colour");
                        ui.color_edit_button_rgb(c);
                    });
                }
                ScreenContent::Image { name, bytes } => {
                    ui.horizontal(|ui| {
                        if ui.button("Choose image…").clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "gif", "webp", "tga", "exr", "hdr"])
                                .pick_file()
                        {
                            match std::fs::read(&path) {
                                Ok(b) => {
                                    *bytes = std::sync::Arc::new(b);
                                    *name = path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("image")
                                        .to_string();
                                }
                                Err(e) => log::error!("read screen image: {e}"),
                            }
                        }
                        let label = if name.is_empty() { "no image".to_string() } else { name.clone() };
                        ui.label(RichText::new(label).weak());
                    });
                }
                ScreenContent::Ndi { source } => {
                    ui.horizontal(|ui| {
                        ui.label("Source");
                        ui.text_edit_singleline(source);
                    });
                    if !sources.ndi.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Discovered:").weak().small());
                            for name in &sources.ndi {
                                if ui.small_button(name).clicked() {
                                    *source = name.clone();
                                }
                            }
                        });
                    }
                    let hint = if sources.ndi_available {
                        "NDI source name (e.g. \"HOST (Output 1)\"). Pick a discovered source above."
                    } else {
                        "NDI runtime not available (build with `--features ndi` + install the NDI runtime)."
                    };
                    ui.label(RichText::new(hint).weak().small());
                }
                ScreenContent::Citp { source } => {
                    ui.horizontal(|ui| {
                        ui.label("Source");
                        ui.text_edit_singleline(source);
                    });
                    if !sources.citp.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Servers:").weak().small());
                            for name in &sources.citp {
                                if ui.small_button(name).clicked() {
                                    *source = name.clone();
                                }
                            }
                        });
                    }
                    ui.label(
                        RichText::new("CITP/MSEX media-server stream as \"server | layer\" (servers auto-discovered on the LAN).")
                            .weak()
                            .small(),
                    );
                }
                ScreenContent::PixelMapDmx(pm) => {
                    Grid::new("led-pixelmap").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                        ui.label("Grid");
                        ui.horizontal(|ui| {
                            let mut c = pm.cols as i32;
                            let mut r = pm.rows as i32;
                            if ui.add(DragValue::new(&mut c).speed(0.1).range(1..=64).prefix("cols ")).changed() {
                                pm.cols = c.max(1) as u32;
                            }
                            if ui.add(DragValue::new(&mut r).speed(0.1).range(1..=64).prefix("rows ")).changed() {
                                pm.rows = r.max(1) as u32;
                            }
                        });
                        ui.end_row();
                        ui.label("Patch");
                        ui.horizontal(|ui| {
                            let mut u = pm.universe as i32;
                            let mut a = pm.start_address as i32;
                            if ui.add(DragValue::new(&mut u).speed(0.1).range(0..=63999).prefix("univ ")).changed() {
                                pm.universe = u.clamp(0, 63999) as u16;
                            }
                            if ui.add(DragValue::new(&mut a).speed(0.5).range(1..=512).prefix("addr ")).changed() {
                                pm.start_address = a.clamp(1, 512) as u16;
                            }
                        });
                        ui.end_row();
                    });
                    let chans = pm.cols * pm.rows * 3;
                    ui.label(
                        RichText::new(format!(
                            "{}×{} cells · {chans} ch (RGB) · low-res only — use NDI/CITP/media for hi-res",
                            pm.cols, pm.rows
                        ))
                        .weak()
                        .small(),
                    );
                }
            }
        },
    );
}
