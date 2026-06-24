//! The **active-tool** model + the viewport tool-rail (`docs/RESEARCH-blender-framework.md` §2.4).
//!
//! Blender's model: the *active tool* — not the selection — decides what a viewport
//! press/drag does and which gizmo group draws. The T-panel toolbar is a radio
//! column generated from the registered tools. We mirror that with [`ActiveTool`]:
//! a single per-viewport mode held on [`super::Ui`] (default [`ActiveTool::Select`]).
//!
//! P3a scope: the enum + its icon/label/tooltip + the `TOOLBAR` order the rail
//! iterates, plus [`ActiveTool::shows_xform_gizmo`] which gates the existing
//! screen-space MOVE gizmo (`panels::viewport`) so it only draws/handles under the
//! Move tool. **Select is the fallback** — a plain click still selects under any
//! tool; only the gizmo presentation changes. Per-MODE tool filtering and the
//! Rotate/Scale/Aim/Measure gizmo groups are later phases (§2.4 trait sketch).

use crate::ui::theme;

/// The viewport's active tool (Blender's `bToolRef`, one per viewport). Decides
/// what a press/drag does + which gizmo draws; default is [`ActiveTool::Select`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ActiveTool {
    /// Plain click-select, no transform gizmo (the fallback tool).
    #[default]
    Select,
    /// Screen-space RGB move gizmo (the only gizmo wired in P3a).
    Move,
    /// Rotate dials — gizmo group is a later phase; rail entry present now.
    Rotate,
    /// Scale boxes — gizmo group is a later phase; rail entry present now.
    Scale,
    /// Aim a selected head at a clicked point (the lighting differentiator) — later phase.
    Aim,
    /// Two-point ruler (never mutates the scene) — later phase.
    Measure,
    /// Click-to-place add — later phase (the Add menu already exists via `Shift+A`).
    Add,
}

impl ActiveTool {
    /// Rail order — the column the T-panel iterates top-to-bottom. Select first
    /// (the fallback), then the transform trio, then the lighting tools.
    pub const TOOLBAR: [ActiveTool; 7] = [
        ActiveTool::Select,
        ActiveTool::Move,
        ActiveTool::Rotate,
        ActiveTool::Scale,
        ActiveTool::Aim,
        ActiveTool::Measure,
        ActiveTool::Add,
    ];

    /// The Phosphor glyph shown on the rail button (semantic, never emoji).
    pub fn icon(self) -> &'static str {
        match self {
            ActiveTool::Select => theme::icon::TOOL_SELECT,
            ActiveTool::Move => theme::icon::TOOL_MOVE,
            ActiveTool::Rotate => theme::icon::TOOL_ROTATE,
            ActiveTool::Scale => theme::icon::TOOL_SCALE,
            ActiveTool::Aim => theme::icon::TOOL_AIM,
            ActiveTool::Measure => theme::icon::TOOL_MEASURE,
            ActiveTool::Add => theme::icon::ADD,
        }
    }

    /// Short human label (used later in the header tool-options strip; part of the
    /// §2.4 API surface, not yet wired — kept for the next phase).
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            ActiveTool::Select => "Select",
            ActiveTool::Move => "Move",
            ActiveTool::Rotate => "Rotate",
            ActiveTool::Scale => "Scale",
            ActiveTool::Aim => "Aim",
            ActiveTool::Measure => "Measure",
            ActiveTool::Add => "Add",
        }
    }

    /// Hover tooltip for the rail button — label + a one-line hint.
    pub fn tooltip(self) -> &'static str {
        match self {
            ActiveTool::Select => "Select — click to pick objects",
            ActiveTool::Move => "Move — drag the RGB axis handles to translate",
            ActiveTool::Rotate => "Rotate — drag a ring to rotate about that axis",
            ActiveTool::Scale => "Scale — drag a box to scale (centre = uniform)",
            ActiveTool::Aim => "Aim — drag to point the selected head(s) at the cursor",
            ActiveTool::Measure => "Measure — click two points for distance (Esc clears)",
            ActiveTool::Add => "Add (coming soon) — use Shift+A for the add menu",
        }
    }

    /// Whether this tool draws a screen-space transform gizmo group at the pivot.
    /// P3b wires Move (arrows), Rotate (rings) and Scale (boxes) — see
    /// [`super::gizmo::for_tool`], the single source that maps tool → group. The
    /// spring-loaded G/R/S modal transforms stay available under every tool.
    pub fn shows_xform_gizmo(self) -> bool {
        matches!(self, ActiveTool::Move | ActiveTool::Rotate | ActiveTool::Scale)
    }
}
