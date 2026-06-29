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
//!
//! C2 (P0 #10 + #11): the per-tool presentation + interaction is now a single
//! declarative [`ToolDef`] table ([`TOOLS`]) — one row per tool carrying its id,
//! label, icon, tooltip, [`GizmoKind`], and [`ToolDef::fallback_op`] (the command
//! that runs when the tool itself doesn't handle a viewport press; **#11**: the
//! "click still selects under any tool" rule is the `select.replace` fallback
//! expressed as DATA, not a hardcoded branch). The rail (`mod::tool_rail`), the
//! gizmo map (`gizmo::for_tool`), and the viewport dispatch (`panels::viewport`)
//! all read this table instead of per-arm `match`es, so adding/retuning a tool is
//! one row. The `icon`/`label`/`tooltip`/`shows_xform_gizmo` accessors are kept as
//! thin lookups over [`ToolDef`] so every existing caller reads identically.

use crate::ui::theme;

/// The viewport's active tool (Blender's `bToolRef`, one per viewport). Decides
/// what a press/drag does + which gizmo draws; default is [`ActiveTool::Select`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
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

/// Which screen-space gizmo group a tool draws at the selection pivot (Blender's
/// `bToolRef.runtime->gizmo_group`, projected to our three wired groups). [`None`]
/// means plain click-select with no gizmo (Select / Aim / Measure / Add).
/// [`super::gizmo::for_tool`] maps these to the concrete [`super::gizmo::GizmoGroup`]
/// implementors — this enum is the data-side handle so the tool table stays free of
/// trait objects (`ToolDef` is a plain `&'static` row).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GizmoKind {
    /// No transform gizmo — the tool relies on its fallback op (click-select).
    None,
    /// Screen-space RGB move arrows (`gizmo::MoveGizmo`).
    Move,
    /// Rotate rings (`gizmo::RotateGizmo`).
    Rotate,
    /// Scale boxes (`gizmo::ScaleGizmo`).
    Scale,
}

/// One declarative row describing a viewport tool (P0 #10): its rail presentation
/// (icon/label/tooltip), the [`GizmoKind`] it draws, and its [`ToolDef::fallback_op`]
/// — the command run when the tool doesn't itself consume a viewport press (#11:
/// "click selects under any tool" as a data field, not a hardcoded branch). The rail
/// iterates [`TOOLS`] top-to-bottom; the gizmo/viewport dispatch resolve a tool's row
/// via [`ActiveTool::def`].
pub struct ToolDef {
    /// The tool this row describes (the discriminant the rest of the app holds).
    pub tool: ActiveTool,
    /// Stable string id (`tool.<verb>`), parallel to the command-registry ids.
    /// Part of the §2.4 API surface (future per-mode tool palettes + remap UI key
    /// off it); pinned-unique by a unit test, not yet read at runtime.
    #[allow(dead_code)]
    pub id: &'static str,
    /// Short human label (header tool-options strip / future remap UI).
    pub label: &'static str,
    /// Phosphor glyph for the rail button (semantic, never emoji).
    pub icon: &'static str,
    /// Hover tooltip — label + a one-line hint.
    pub tooltip: &'static str,
    /// The transform gizmo group this tool draws, if any.
    pub gizmo: GizmoKind,
    /// The command id run when a viewport press isn't consumed by the tool's own
    /// behaviour. Every tool falls back to `select.replace` (#11: click-select is
    /// universal), expressed here as data rather than a hardcoded branch.
    pub fallback_op: Option<&'static str>,
}

/// The declarative tool table (P0 #10). Rail order is the slice order: Select first
/// (the fallback), then the transform trio, then the lighting tools. Every accessor
/// and the rail/gizmo/viewport dispatch read this single source.
pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        tool: ActiveTool::Select,
        id: "tool.select",
        label: "Select",
        icon: theme::icon::TOOL_SELECT,
        tooltip: "Select — click to pick objects",
        gizmo: GizmoKind::None,
        fallback_op: Some("select.replace"),
    },
    ToolDef {
        tool: ActiveTool::Move,
        id: "tool.move",
        label: "Move",
        icon: theme::icon::TOOL_MOVE,
        tooltip: "Move — drag the RGB axis handles to translate",
        gizmo: GizmoKind::Move,
        fallback_op: Some("select.replace"),
    },
    ToolDef {
        tool: ActiveTool::Rotate,
        id: "tool.rotate",
        label: "Rotate",
        icon: theme::icon::TOOL_ROTATE,
        tooltip: "Rotate — drag a ring to rotate about that axis",
        gizmo: GizmoKind::Rotate,
        fallback_op: Some("select.replace"),
    },
    ToolDef {
        tool: ActiveTool::Scale,
        id: "tool.scale",
        label: "Scale",
        icon: theme::icon::TOOL_SCALE,
        tooltip: "Scale — drag a box to scale (centre = uniform)",
        gizmo: GizmoKind::Scale,
        fallback_op: Some("select.replace"),
    },
    ToolDef {
        tool: ActiveTool::Aim,
        id: "tool.aim",
        label: "Aim",
        icon: theme::icon::TOOL_AIM,
        tooltip: "Aim — drag to point the selected head(s) at the cursor",
        gizmo: GizmoKind::None,
        fallback_op: Some("select.replace"),
    },
    ToolDef {
        tool: ActiveTool::Measure,
        id: "tool.measure",
        label: "Measure",
        icon: theme::icon::TOOL_MEASURE,
        tooltip: "Measure — click two points for distance (Esc clears)",
        gizmo: GizmoKind::None,
        fallback_op: Some("select.replace"),
    },
    ToolDef {
        tool: ActiveTool::Add,
        id: "tool.add",
        label: "Add",
        icon: theme::icon::ADD,
        tooltip: "Add (coming soon) — use Shift+A for the add menu",
        gizmo: GizmoKind::None,
        fallback_op: Some("select.replace"),
    },
];

impl ActiveTool {
    /// Rail order — the column the T-panel iterates top-to-bottom. Derived from
    /// [`TOOLS`] so the order lives in one place; Select first (the fallback), then
    /// the transform trio, then the lighting tools.
    pub const TOOLBAR: [ActiveTool; 7] = [
        ActiveTool::Select,
        ActiveTool::Move,
        ActiveTool::Rotate,
        ActiveTool::Scale,
        ActiveTool::Aim,
        ActiveTool::Measure,
        ActiveTool::Add,
    ];

    /// This tool's [`ToolDef`] row. Every tool has exactly one row in [`TOOLS`]
    /// (pinned by a unit test), so the lookup never fails in practice; the fallback
    /// row (`Select`) keeps it total.
    pub fn def(self) -> &'static ToolDef {
        TOOLS.iter().find(|d| d.tool == self).unwrap_or(&TOOLS[0])
    }

    /// The Phosphor glyph shown on the rail button (semantic, never emoji).
    pub fn icon(self) -> &'static str {
        self.def().icon
    }

    /// Short human label (used later in the header tool-options strip; part of the
    /// §2.4 API surface, not yet wired — kept for the next phase).
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        self.def().label
    }

    /// Hover tooltip for the rail button — label + a one-line hint.
    pub fn tooltip(self) -> &'static str {
        self.def().tooltip
    }

    /// The command id run when a viewport press isn't consumed by this tool's own
    /// behaviour (#11). `None` would mean "no fallback"; today every tool falls back
    /// to click-select. Part of the §2.4 API surface — the viewport's click-select
    /// path is the consumer of this rule.
    #[allow(dead_code)]
    pub fn fallback_op(self) -> Option<&'static str> {
        self.def().fallback_op
    }

    /// Whether this tool draws a screen-space transform gizmo group at the pivot.
    /// P3b wires Move (arrows), Rotate (rings) and Scale (boxes) — see
    /// [`super::gizmo::for_tool`], the single source that maps tool → group. The
    /// spring-loaded G/R/S modal transforms stay available under every tool. Reads
    /// the tool's [`GizmoKind`] from [`TOOLS`].
    pub fn shows_xform_gizmo(self) -> bool {
        self.def().gizmo != GizmoKind::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P0 #10: every `ActiveTool` has exactly one `ToolDef` row in `TOOLS`, and the
    /// table covers nothing else — the table is the total, unambiguous source.
    #[test]
    fn every_tool_has_exactly_one_def() {
        for tool in ActiveTool::TOOLBAR {
            let n = TOOLS.iter().filter(|d| d.tool == tool).count();
            assert_eq!(
                n, 1,
                "{tool:?} must have exactly one ToolDef row, found {n}"
            );
        }
        assert_eq!(
            TOOLS.len(),
            ActiveTool::TOOLBAR.len(),
            "TOOLS and the rail order must list the same tools"
        );
    }

    /// The rail order and the table order must agree (the rail derives its column
    /// from `TOOLBAR`, the gizmo/viewport dispatch from `TOOLS`).
    #[test]
    fn toolbar_matches_table_order() {
        for (i, tool) in ActiveTool::TOOLBAR.iter().enumerate() {
            assert_eq!(
                *tool, TOOLS[i].tool,
                "rail order diverges from TOOLS at {i}"
            );
        }
    }

    /// Tool ids are unique (mirrors the command-registry id contract).
    #[test]
    fn tool_ids_unique() {
        for (i, a) in TOOLS.iter().enumerate() {
            for b in &TOOLS[i + 1..] {
                assert_ne!(a.id, b.id, "duplicate tool id {}", a.id);
            }
        }
    }

    /// Parity: the gizmo-bearing tools are exactly Move/Rotate/Scale (the prior
    /// `shows_xform_gizmo` truth), now resolved through the table.
    #[test]
    fn gizmo_tools_unchanged() {
        for tool in ActiveTool::TOOLBAR {
            let expected = matches!(
                tool,
                ActiveTool::Move | ActiveTool::Rotate | ActiveTool::Scale
            );
            assert_eq!(
                tool.shows_xform_gizmo(),
                expected,
                "{tool:?} gizmo gate changed"
            );
        }
    }

    /// #11: every tool falls back to click-select, as data.
    #[test]
    fn all_tools_fall_back_to_click_select() {
        for tool in ActiveTool::TOOLBAR {
            assert_eq!(
                tool.fallback_op(),
                Some("select.replace"),
                "{tool:?} fallback changed"
            );
        }
    }
}
