use super::*;

impl Ui {
    pub fn new() -> Self {
        // Load the user's workspaces (built-ins if the file is absent); the active
        // one's saved dock layout is the startup layout and its default tool the
        // startup tool. Overlay flags are deliberately LEFT at the user's saved
        // Preferences defaults here — they're only re-emphasised on an explicit
        // workspace ACTIVATION (a user switch), so startup prefs stay deterministic.
        let workspaces = workspaces::Workspaces::load();
        let active = workspaces.active().clone();
        Self {
            dock: active.dock.clone(),
            library: Library::standard(),
            gdtf_textures: HashMap::new(),
            selection: Selection::fixture(0),
            settings: RenderSettings::default(),
            prefs: Preferences::default(),
            requested_viewport_px: (1, 1),
            viewport_visible: false,
            render: RenderUiState::default(),
            pending_fullscreen_toggle: false,
            render_active: false,
            viewport_focused: false,
            duplicate: None,
            replace: None,
            pending_replace: false,
            // Debug hook (S2): GLOWSTONE_UI_PREFS opens the Preferences window at
            // startup so the headless GLOWSTONE_UI screenshot can capture the keymap
            // editor without app.rs (off-limits) needing a dedicated flag.
            show_prefs: std::env::var_os("GLOWSTONE_UI_PREFS").is_some(),
            show_about: false,
            show_shortcuts: false,
            show_perf: std::env::var("GLOWSTONE_PERF").is_ok(),
            profile: None,
            lib: library::LibState::default(),
            scene_anchor: None,
            scene_sort: outliner::SceneSort::Patch,
            scene_search: String::new(),
            scene_filter: tree::OutlinerFilter::default(),
            scene_expanded: {
                // Sensible default expand-state: the project + its top groups open,
                // so fixtures are visible at a glance (and the headless screenshot
                // is deterministic).
                use tree::{GroupKind, NodeKey};
                let mut s = std::collections::HashSet::new();
                s.insert(NodeKey::Root);
                s.insert(NodeKey::World);
                s.insert(NodeKey::EnvGroup);
                s.insert(NodeKey::Group(GroupKind::Devices));
                s.insert(NodeKey::Group(GroupKind::Objects));
                s
            },
            scene_rename: None,
            pending_tree: tree::TreeAction::None,
            screen_sources: panels::ScreenSources::default(),
            fm: panels::FmState::default(),
            quick_select: false,
            add_menu: windows::AddMenuState::default(),
            op_search: windows::OperatorSearchState::default(),
            patch_dialog: windows::PatchDialog::default(),
            unpatch_dialog: windows::UnpatchDialog::default(),
            transform: None,
            transform_before: None,
            transform_started: false,
            transform_finished: false,
            inspector_tx: None,
            inspector_edit: panels::InspectorEdit::default(),
            inspector_state: inspector::InspectorState::load(),
            last_nudge: None,
            pending_nudge: Vec3::ZERO,
            groups: Vec::new(),
            cues: cues::CueEngine::default(),
            undo: op::UndoStack::default(),
            pending_delete: false,
            pending_lib_add: false,
            share: crate::share::Share::new(),
            show_share: false,
            current_path: None,
            saved_state_id: 0,
            box_select_armed: false,
            show_splash: true,
            welcome_tex: None,
            recent: project::load_recent(),
            autosave_timer: 0.0,
            viewport_regions: ViewportRegions::default(),
            active_tool: active.default_tool,
            xform: TransformPrefs::default(),
            cursor_3d: Vec3::ZERO,
            cursor_3d_set: false,
            lib_prefs: lib_prefs::LibraryPrefs::load(),
            bookmarks: bookmarks::Bookmarks::load(),
            workspaces,
            save_workspace: None,
            keymap_overrides: shortcuts::KeymapOverrides::load(),
            keymap_editor: windows::KeymapEditorState::default(),
            measure: panels::MeasureState::default(),
            aim: panels::AimState::default(),
            view_pie: pie::PieState::default(),
            shading_pie: pie::PieState::default(),
            notify: notify::Notifier::default(),
            status_msgs: notify::StatusStack::default(),
            // Debug hook (S3): GLOWSTONE_UI_LOG opens the report-log window at startup
            // so the headless GLOWSTONE_UI screenshot can capture it (mirrors the
            // GLOWSTONE_UI_PREFS prefs-window hook) without touching app.rs.
            show_report_log: std::env::var_os("GLOWSTONE_UI_LOG").is_some(),
            dmx_was_running: false,
        }
    }

    /// The default dock layout (also used by Window ▸ Reset Panel Layout).
    ///
    /// egui_dock's `fraction` is the share given to the side being split toward:
    /// `split_left(n, f)` makes the NEW left panel `f` of the width, `split_right`
    /// makes the new right panel `1 - f`, `split_below` the new bottom `1 - f`.
    /// (The old code passed 0.80 to `split_left` expecting the central to keep
    /// 80% — that made the Scene sidebar 80% wide, the startup-layout bug.)
    pub(super) fn default_dock(&self) -> DockState<Tab> {
        self.workspaces.active().dock.clone()
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}
