use super::*;

/// What the welcome splash's buttons request (applied after the modal closure,
/// where `scene` / `dmx` are mutably reachable again).
enum SplashAction {
    New,
    Open,
    OpenPath(PathBuf),
    Recover(PathBuf),
    Dismiss,
}

impl Ui {
    /// Dismiss the welcome splash (headless screenshot hook).
    pub fn dismiss_splash(&mut self) {
        self.show_splash = false;
    }

    /// Open the welcome splash — Window ▸ Welcome, the New command, and the operator
    /// search all route here.
    pub fn show_welcome(&mut self) {
        self.show_splash = true;
    }

    /// The welcome hero image, decoded from the bundled JPEG and uploaded to the GPU
    /// once (lazily, then cached). `None` only if the embedded image fails to decode.
    fn welcome_texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.welcome_tex.is_none() {
            static BYTES: &[u8] = include_bytes!("welcome.jpg");
            // Cap the longest side well under the GPU/egui max-texture-side (2048) —
            // the splash shows it ~580 px wide, so this never costs visible quality and
            // guards against a "texture too large" panic if the asset is ever swapped.
            // `resize` only ever downscales (it preserves aspect, fitting the box).
            let img = image::load_from_memory(BYTES)
                .ok()?
                .resize(1600, 1600, image::imageops::FilterType::Lanczos3)
                .to_rgba8();
            let (w, h) = img.dimensions();
            let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
            self.welcome_tex = Some(ctx.load_texture("welcome-hero", color, egui::TextureOptions::LINEAR));
        }
        self.welcome_tex.clone()
    }

    /// The Blender-style welcome / recover splash, shown once at startup.
    pub(super) fn splash_window(
        &mut self,
        ctx: &egui::Context,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        if !self.show_splash {
            return;
        }
        let mut action: Option<SplashAction> = None;
        // Pull the hero texture + recent list out before the modal closure so the
        // closure borrows neither `self` (welcome_texture needs &mut self) nor the
        // recent Vec mutably.
        let hero = self.welcome_texture(ctx);
        let recent: Vec<PathBuf> = self.recent.iter().take(8).cloned().collect();
        let autosave = project::autosave_path().filter(|p| p.exists());
        let modal = egui::Modal::new(egui::Id::new("welcome-splash")).show(ctx, |ui| {
            ui.set_width(580.0);
            // ---- hero image (full width) with the wordmark + version overlaid on a
            // bottom scrim, like Blender's splash artwork. ----
            if let Some(tex) = &hero {
                let w = ui.available_width();
                let sz = tex.size();
                let h = w * sz[1] as f32 / sz[0] as f32;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
                let painter = ui.painter_at(rect);
                // Round ALL FOUR corners so the artwork reads as a rounded card inside
                // the modal (the dialog itself rounds at ~7px). The bottom corners must
                // round too — the semi-transparent scrim can't hide a square image
                // corner behind it.
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                let radius = egui::CornerRadius::same(6);
                painter.add(
                    egui::epaint::RectShape::filled(rect, radius, egui::Color32::WHITE)
                        .with_texture(tex.id(), uv),
                );
                let scrim = egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.bottom() - 46.0),
                    rect.right_bottom(),
                );
                // Match the dark strip to the image's bottom edge: round its BOTTOM
                // corners by the same radius so the scrim hugs the rounded outline.
                let bottom = egui::CornerRadius { nw: 0, ne: 0, sw: 6, se: 6 };
                painter.add(egui::epaint::RectShape::filled(
                    scrim,
                    bottom,
                    egui::Color32::from_black_alpha(150),
                ));
                painter.text(
                    egui::pos2(rect.left() + 16.0, rect.bottom() - 23.0),
                    egui::Align2::LEFT_CENTER,
                    "glowstone",
                    egui::FontId::proportional(26.0),
                    egui::Color32::WHITE,
                );
                painter.text(
                    egui::pos2(rect.right() - 16.0, rect.bottom() - 23.0),
                    egui::Align2::RIGHT_CENTER,
                    format!("v{} alpha", env!("CARGO_PKG_VERSION")),
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_white_alpha(210),
                );
            } else {
                // Fallback if the bundled image fails to decode: the plain text header.
                ui.horizontal(|ui| {
                    ui.heading("glowstone");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("v{} alpha", env!("CARGO_PKG_VERSION")))
                                .weak()
                                .small(),
                        );
                    });
                });
            }

            ui.add_space(14.0);
            ui.columns(2, |cols| {
                // Left — start a session.
                cols[0].label(egui::RichText::new("New").strong());
                cols[0].add_space(6.0);
                if cols[0]
                    .add_sized([240.0, 30.0], egui::Button::new(format!("{}  New Project", theme::icon::SCENE)))
                    .clicked()
                {
                    action = Some(SplashAction::New);
                }
                if cols[0]
                    .add_sized([240.0, 30.0], egui::Button::new(format!("{}  Open…", theme::icon::IMPORT_MVR)))
                    .clicked()
                {
                    action = Some(SplashAction::Open);
                }
                if let Some(ap) = &autosave {
                    cols[0].add_space(10.0);
                    if cols[0]
                        .add_sized([240.0, 30.0], egui::Button::new(format!("{}  Recover Last Session", theme::icon::FRAME)))
                        .on_hover_text("Reopen the auto-saved session from the last run")
                        .clicked()
                    {
                        action = Some(SplashAction::Recover(ap.clone()));
                    }
                }

                // Right — recent files.
                cols[1].label(egui::RichText::new("Recent Files").strong());
                cols[1].add_space(6.0);
                if recent.is_empty() {
                    cols[1].label(egui::RichText::new("No recent projects").weak().small());
                } else {
                    for p in &recent {
                        let name = p
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if cols[1]
                            .add(egui::Button::new(format!("{}  {}", theme::icon::PROFILE, name)).frame(false))
                            .on_hover_text(p.display().to_string())
                            .clicked()
                        {
                            action = Some(SplashAction::OpenPath(p.clone()));
                        }
                    }
                }
            });

            ui.add_space(12.0);
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Continue without a project").clicked() {
                    action = Some(SplashAction::Dismiss);
                }
            });
        });

        if modal.should_close() {
            self.show_splash = false;
        }
        match action {
            Some(SplashAction::New) => self.new_project(scene, camera, dmx),
            Some(SplashAction::Open) => self.open_project_dialog(scene, camera, dmx),
            Some(SplashAction::OpenPath(p)) => self.open_project(&p, scene, camera, dmx),
            Some(SplashAction::Recover(p)) => {
                self.open_project(&p, scene, camera, dmx);
                // A recovered session is untitled — don't let Save clobber the
                // autosave file; force Save As on the next save.
                self.current_path = None;
            }
            Some(SplashAction::Dismiss) => self.show_splash = false,
            None => {}
        }
    }
}
