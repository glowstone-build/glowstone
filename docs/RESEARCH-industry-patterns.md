# Industry UX/Feature Patterns for previz — Blender + Unreal, synthesized

*Mining two industry-standard 3D editors (Blender = GPL, Unreal = proprietary EULA) for
UX/feature patterns to port — as our own MIT/Apache Rust — into **previz**, a Rust + wgpu +
egui(egui_dock) arch-viz / lighting-previsualization tool (stage rigs, DMX/GDTF fixtures,
beams/haze, MVR scenes).*

> **License discipline.** We REIMPLEMENT behaviour/design in our own code. Source files are
> cited only as *behaviour references* (symbol + line). Never copy GPL/EULA source. Patterns
> and ideas are fine; code is not.

> **Scope discipline.** previz is NOT a game engine and NOT a general DCC. Every "skip" below
> is a game-only / animation-only / DCC-only pattern we deliberately reject. See §4.

Companion to [`RESEARCH-blender-framework.md`](RESEARCH-blender-framework.md) — see §5 for how
this layers onto that phased plan (especially Phase 4 properties / Phase 5 modes+keymap).

---

## 1. Executive summary — the highest-leverage "best of both" moves

These 15 moves cut across the 11 domains and are ordered by leverage (structural payoff ×
breadth of features they unblock). Each is detailed in §2 and itemized in §3.

1. **Smooth/animated camera transitions for every canned-view, frame, and bookmark jump.**
   Our `set_view`/`frame` snap instantly; both engines animate. Single highest-impact,
   lowest-risk viewport upgrade. *(Blender `SmoothView3DState` concept + Unreal `FCurveSequence`
   CubicOut lerp.)* → `camera.rs`, `mod.rs:1753`.

2. **One `SelectOp` enum + pure per-element truth-table** (Replace/Add/Sub/Toggle → {select,
   deselect, leave}) driving click AND box AND lasso. Collapses the duplicated per-`Hit`-arm
   modifier logic in `panels.rs`. *(Blender `ED_select_op_action` — strictly better than our
   scattered branches.)*

3. **Box/marquee select in the viewport** (CPU screen-projected bounds, loose by default,
   `eSelectOp`-driven). Biggest single missing selection feature vs both engines. → `panels.rs`.

4. **Delta-compressed undo snapshots.** Hash each sub-blob (scene/patch/cues/groups) and
   `Arc`-share unchanged blobs across adjacent steps. A one-fixture move on a 500-fixture rig
   should store ~one fixture's bytes, not the whole scene. *(Blender memfile `is_identical`
   chunk-sharing.)* → `op.rs` `DocSnapshot`.

5. **Interactive begin→preview→finalize transaction path** for gizmo drags and inspector
   slider drags: one undo step per gesture, live preview, restore-on-Esc. *(Unreal
   `SnapshotObject`/`FScopedTransaction`.)* → `op.rs` + `mod.rs:403` `transform_before`.

6. **Named-registry menu/toolbar system + unified Command descriptor.** Every menu/header/rail
   gets a stable string id; features register entries by anchor (Before/After + missing-anchor
   fallback) instead of editing one monolith; a Command's label+icon+default-chord is defined
   once and read by keymap, menu, toolbar, and the F3 palette. *(Unreal `UToolMenus` +
   `FUICommandInfo` — the single most valuable structural port.)* → merges `shortcuts.rs` +
   `op.rs` CATALOG.

7. **Bind keymap items to operator `id` strings (not the closed `Action` enum) + diff-based
   user keymap persistence.** Prerequisite for any remap UI; store only user deltas over the
   shipped default so default-shortcut evolution is non-destructive. *(Blender
   `wm_keymap_diff`/`patch` + Unreal `FUserDefinedChords`.)* → `shortcuts.rs`, prefs.

8. **Per-property reset-to-default + true multi-edit + Simple/Advanced split** in the inspector.
   "Default" = the GDTF/library template value (genuinely meaningful for fixtures); detect
   mixed values across a multi-selection and only write touched fields; tuck power-user rows
   behind an "Advanced ▾" caret. *(Unreal `SResetToDefaultPropertyEditor` + `GetReadAddress`/
   `bAllValuesTheSame` + `EPropertyLocation::Common/Advanced`.)* → `panels.rs` inspectors.

9. **Pivot-point + transform-orientation selectors.** Generalize the hardcoded centroid pivot
   to Median / 3D-Cursor / Individual-Origins / Active, and add a Global/Local/View orientation
   basis. Individual-origins (fan a row of heads) and Local (head on a raked truss) are the
   previz must-haves. *(Blender `gizmo_3d_calc_pos` + `applyTransformOrientation`.)* →
   `mod.rs:207` `TransformOp.pivot`, `panels.rs:1941` `apply_transform`.

10. **Grid/increment snap + ray-plane absolute drag.** Header snap toggle + per-axis
    `step*round(v/step)` applier; replace the pixel-delta heuristic with eye-ray vs pivot-plane
    intersection so the handle sticks to the cursor at any camera angle. *(Blender
    `snap_increment_apply` + Unreal `GetAbsoluteTranslationDelta`.)* → `panels.rs:1941`.

11. **User folders as path strings + 3-band drag-reparent.** `folder: String` ("Lighting/SR/
    Truss 1") on entities (serde-default, no breaking bump) + a folder-color map; before/after/
    into drop zones by cursor-Y with cycle guard and live validation text. Far cheaper than
    Blender's Collection datablock graph and fits our flat Scene. *(UE `FFolder` path model +
    Blender `outliner_drop_insert_find`.)* → `tree.rs`, `scene/mod.rs`.

12. **Transient toast + report-log system (we have none) + handle-based status-bar message
    stack + structured modal-hint HUD.** Every `report(severity, msg, opt action)` is both a
    fading toast and a permanent log entry; tools push/pop status messages by handle; G/R/S
    advertise their live keys in a viewport pill read from the keymap. *(Unreal
    `FNotificationInfo`/`PushStatusBarMessage` + Blender `BKE_report`/`WorkspaceStatus`.)* →
    `mod.rs:2181` `status_bar`, new module.

13. **Place at cursor/camera (not origin) + Recent/Favourites pseudo-categories + fuzzy/recent
    search.** Drops currently land at `(0,4,0)`; raycast the ground plane or place in front of
    the camera, wrapped in one undo op. Recent (front-insert, de-dupe, cap 20, persist) and a
    fuzzy+recency scorer replace cold first-scroll and substring-only filtering across Library
    AND the Shift+A Add menu. *(Unreal PlacementMode recipe + Blender `BLI_string_search`.)* →
    `panels.rs:432`, `add_menu.rs`.

14. **Modes as registered objects (Layout/Patch/Focus/Render) with per-mode tool palettes +
    remembered last tool.** A mode swaps the T-panel rail and restores its last active tool;
    the tool itself becomes a declarative `ToolDef` row (icon/cursor/gizmo/op/**fallback op**)
    instead of scattered match arms — with click-selects-under-any-tool as one data field.
    *(Unreal `FEditorModeInfo` + Blender `ToolDef`/`keymap_fallback`.)* → `tools.rs`,
    `editor.rs`. Aligns with the locked MEMORY modes decision.

15. **F3 poll-filtered command palette + inline "Adjust Last Operation".** A fuzzy list over
    CATALOG showing only ops whose `can_execute` passes now, each with its live shortcut
    (doubles as a cheat-sheet); F9 pops the last step and re-execs with edited params instead
    of re-opening the whole dialog. *(Blender `operator_search_update_fn` + `ED_undo_operator_
    repeat`.)* → `op.rs` CATALOG, `shortcuts.rs` `OpSearch`/`AdjustLast`.

**Count:** the prioritized backlog (§3) holds **23 P0**, **34 P1**, plus P2/P3 — ~90 concrete,
specific features grounded in the source analyses.

---

## 2. Per-domain winning designs

Each domain states the best-of-both design (which engine does it better + why), then the
concrete previz features. Full priority/effort/seam in the §3 backlog.

### 2.1 Outliner / scene hierarchy

**Winner: Blender's *separation of concerns* (distinct hide / select-lock / render flags,
solid-vs-dashed hierarchy lines, color tags, keep-ancestors search, isolate/solo) + Unreal's
*data-driven plumbing* (pluggable `ColumnMap`, `FFolder` path strings, selection-aware +
drag-paint visibility, live drop-validation text).** Blender proves *which* flags matter for 3D
scenes; Unreal proves the column/folder infra should be data-driven so we can add a "patched"
or "beam-on" column later without relayout. Our `tree.rs` already derives rows per-frame with
separate persisted expand state (the one generic Blender idea worth keeping) and models
`VisState::Mixed`.

Concrete: selection-aware + Shift-recursive eye toggle (click-on-selected toggles all selected;
Shift-click toggles a whole subtree); transparent-until-hover eye for a quiet gutter on big
rigs; user folders as path strings with 3-band drag-reparent + cycle guard + validation text;
pluggable lock/select-disable columns gated behind a header toggle; isolate/solo shortcut;
search that keeps the folder path + highlights the matched substring; type/state filter chips
(Fixtures/Unpatched/Selected/Conflicts); folder color tags; solid-vs-dashed lines; drag-paint
visibility sweep.

### 2.2 Properties / Inspector

**Winner: Unreal's vertical collapsing *category stack* with Simple/Advanced split, read-address
multi-edit, per-property reset-to-default, and `FDetailFilter` as one serializable struct;
Blender's button-level search (label + tooltip + enum items) with auto-expand/auto-dim and its
pin-to-datablock.** Our inspector already uses a vertical `CollapsingHeader` stack, so Unreal's
category model is the natural fit and scales past Blender's horizontal context-tab strip. The
highest-value missing piece is the Simple+Advanced split; the most correctness-relevant is true
multi-edit (our current `bulk_inspector` seeds from the first fixture and silently overwrites).

Concrete: per-property reset arrow (default = GDTF/library template value), shown only when the
field differs; true multi-edit ("Multiple"/"—" placeholder, write only touched fields);
Simple/Advanced caret inside each category; inspector search that matches labels+tooltips+enum
options and auto-expands matches; semantic category ordering (Transform first); right-click
Copy/Paste/Reset; show-only-modified toggle; pin-inspector-to-fixture; EditCondition-style
gray-out (gray "Beam angle" when Beam=0); favorites (defer); section chips (defer).

### 2.3 Camera / navigation / layouts

**Winner: Blender's *interpolation discipline* (animate the quaternion so persp↔ortho axis
flips are smooth, zoom-to-cursor, on-screen nav gizmo, walk mode) + Unreal's *simpler machinery
and persistence* (single eased `FCurveSequence` lerp, dual persp/ortho framing, numbered
bookmarks, persisted multi-pane quad layouts, distance-scaled speed).** Blender proves you
*should* interpolate; Unreal proves one eased start→desired lerp is enough.

Concrete: smooth transitions for all canned-view/frame/bookmark jumps; zoom-to-cursor (reuse
the selection raycast for the anchor point); frame-selected aspect correction; numbered camera
bookmarks persisted in `.archie` (FOH / side / truss shots); on-screen nav gizmo cluster
(draggable axis-ball + pan/zoom/persp-ortho/frame); quad/split layouts (plan + section + side +
3D) via egui_dock split nodes, each pane its own camera+projection; walk mode (WASD+QE, eye
height, optional gravity) for sightline checks; letter-key axis bindings + press-again-for-
opposite; distance-scaled shared speed helper; center-pick orbit pivot + FOV/clip in the
N-panel.

### 2.4 Gizmos / transform / snap

**Winner: Unreal's *robust drag math + centralized snap policy* (ray-plane absolute drag with
grab-offset, dot-product axis projection, `FSnappingUtils` static gates, per-type grid sizes) +
Blender's *pivot/orientation richness + snap-the-constraint-space-delta math + numeric entry*
(`gizmo_3d_calc_pos`, `applyTransformOrientation`, `snap_increment_apply`, expression+unit
`user_string_to_number`).** Our gizmo uses a pixel-delta heuristic that drifts at grazing angles
and only world axes; both gaps are addressed by adopting UE's ray-plane drag + Blender's pivot/
orientation/snap.

Concrete: grid/increment snap toggle + applier; pivot selector (Median / 3D-Cursor / Individual-
Origins / Active); orientation selector (Global/Local/View); expression+unit numeric entry
("1m", "45deg", "1.2*2", "3/2"); plane-constraint handles + double-tap-axis plane lock; ray-
plane absolute drag; snap-target preview marker; snap-to-scene-vertex/truss-node; screen-axis
rotate ring + view-plane move centre; per-type snap increments (0.25m / 15° / 10%).

### 2.5 Modes / tools / toolbars / menus

**Winner: Unreal's *name-addressable extensibility* (`UToolMenus` RegisterMenu/ExtendMenu/
AddSection with positional insertion + missing-anchor fallback; `FEditorModeInfo` registered
modes owning their toolbar; per-mode multi-palette toolbars; PlacementMode category/item
registry) + Blender's *clean tool data model and fallback interaction* (declarative `ToolDef`
table; `keymap_fallback` so plain-click always selects under any tool; mode-is-derived-from-
context).** The named-registry menu system is the single most valuable port for keeping menu
code from becoming a monolith as cues/DMX/MVR grow.

Concrete: named-registry menu/toolbar + unified Command; declarative `ToolDef` table replacing
`ActiveTool` match arms; per-tool fallback op (click-selects under any tool) as a first-class
field; modes as registered objects with per-mode tool rail + remembered last tool; per-mode
tool palettes in the T-panel; dynamic/context menus built from selection; Library as a category/
item placement registry; tool-init gizmo flag; event-driven active-tool refresh.

### 2.6 Undo / operators / commands

**Winner: keep our full-document `DocSnapshot` (Blender's memfile proves it scales and sidesteps
Unreal's pervasive must-call-`Modify()` footgun) but adopt Blender memfile's *delta chunk-
sharing* + Unreal's *scoped/interactive transaction ergonomics* (`FScopedTransaction` RAII,
`SnapshotObject` preview-then-finalize, ref-counted nesting collapse, undo barriers) + Blender's
*F3 poll-filtered search* and *F9 adjust-last* + Unreal's *single-source command model*.**

Concrete: delta-compress `DocSnapshot` (`Arc<[u8]>` + per-blob hash); interactive begin→preview→
finalize path for drags; RAII `Transaction` guard so call sites can't forget the push;
generalized coalescing via an `undo_group` string; F3 command palette; inline F9 Adjust-Last;
nested-op undo-depth guard; unify Kmi + CatalogOp into one descriptor; held-key repeat guard;
formalize the stable-reference revalidation hook (`ensure_ids`/gdtf-Arc reattach).

### 2.7 Input / keymap / prefs

**Winner: Blender's *diff/patch persistence with stable item ids* (store only user deltas;
recombine default+user on load) + parameterized presets + modal-keys-as-data; Unreal's *3-layer
command/context/chord model, three-tier precedence (default < project/studio < user), typed
masking-relationship conflict detection, and dual chord slots per command*.** Lighting designers
share show files and house keymaps, so UE's project/studio tier is directly relevant; Blender's
diff persistence keeps our evolving defaults non-destructive.

Concrete: bind to op `id` (+props) not the closed enum; diff-based user keymap persistence;
keymap remapping UI (per-row + per-section Reset); live conflict detection with a masking-
relationship check; two shipped presets (previz-native G/R/S + Industry-Compatible Maya/UE
W/E/R, LMB-select, RMB-menu); dual chord slots (fold "(alias)" rows); modal keys in the same
editable table; project/studio keymap tier; extended trigger vocab (Press/Release/Click/
DblClick/Drag + repeat-ignore); searchable remap list + show-shortcut-in-menus.

### 2.8 Theming / docking / layout

**Winner: Unreal's *semantic token model* (`EStyleColor` flat role enum resolved through one
live runtime table; serializable `FTabManager::FLayout`; tab-spawner registry; sidebar drawers;
single application scale) + Blender's *procedural state derivation and two-axis theming*
(`widget_active_color` HSL-multiply for hover from a tiny base palette; global-widget + per-
editor split; `UI_SCALE_FAC` woven through every pixel literal).** Promote our ad-hoc `Ink`/
`OK`/`WARN`/`CONFLICT` names to a real enum-indexed Palette; derive hover/press/disabled instead
of storing every permutation.

Concrete: semantic token `Palette` (enum-indexed, swappable); procedural hover/press/disabled
`WidgetVisuals` from one base; single continuous `ui_scale` density pref (+ `pixelsize` for
hairlines); serializable dock layout persisted in prefs + `.archie`; tab-spawner registry that
auto-builds the Window menu; two-axis theming (shared widget palette + small per-editor canvas
overrides matching `gizmo.rs` axis colors); collapsible edge drawers (defer); user-savable named
workspaces; style bundles for bespoke widgets; per-widget roundness/gradient tokens.

### 2.9 Content library / placement

**Winner: Unreal's *placement recipe and filter UX* (place-in-front-of-camera in one undo step;
Recent list front-insert/de-dupe/cap-20/persist; coloured `FFrontendFilter` pills that AND
together; typed `FAssetDragDropOp`; List/Tile toggle with remembered thumbnail size) + Blender's
*search core and lazy previews* (`BLI_string_search` fuzzy+weighted+RecentCache; catalog subtree-
filter; only-render-previews-for-visible-tiles; snap-cursor drag preview).**

Concrete: place at cursor/camera not origin (wrap add+place in one op); Recent + Favourites
pseudo-categories in Library AND Add menu; fuzzy + recency-weighted scorer (shared helper);
filter pills for fixture type/flags; drag-from-Library-into-viewport with ground-plane ghost
+ drop; List↔Grid toggle with lazy thumbnails; manufacturer/category catalog tree (reuse
`tree.rs`); saved fixture "kits"; optional key:value search terms.

### 2.10 Status bar / notifications / overlays

**Winner: Unreal's *handle-based message stack, integer-work-unit aggregated progress, toast
lifecycle (Pending→Success/Fail in place), and provider-keyed actionable in-viewport warnings*
+ Blender's *report severity+log duality, structured operator-owned modal hints
(`WorkspaceStatus`), ETA tooltip, and always-present cancel*.** We have NO toast/report system
today — this is a clean-slate, high-value addition. The provider-keyed actionable warning
(distinct from a fading toast) is the right model for previz lint.

Concrete: handle-based status-bar message stack + grey hint slot; transient toast system with
severity + action + permanent log history; structured modal-hint HUD pill read from the keymap;
provider-keyed actionable warnings with a fix button (missing GDTF→[Relink], universe
conflict→[Open Patch], MVR dropped models→[Show]); async progress slot (done/total, ETA, cancel
X) for Share downloads / save-load / MVR-3DS parse / autosave; configurable status digest
(memory/VRAM/version + selected/total counts); context-sensitive cursor hint; cursor-as-progress;
named realtime-override reason badge.

### 2.11 Selection systems

**Winner: Blender's *`eSelectOp` core, click-cycling, select-passthrough, and deferred dirty-
flag outliner sync* + Unreal's *single-source-of-truth selection set and CAD modifier convention
(plain=replace, Ctrl=toggle, Shift=add-only) + domain select-similar operators*.** Adopt UE's
modifier convention (matches the file-manager mental model our Library/Scene panels already use
with Shift=range/Ctrl=toggle) but implement it via Blender's `eSelectOp` indirection so click/
box/lasso are consistent for free. Keep CPU ray-pick for v1; skip the GPU id-buffer.

Concrete: refactor click+box onto one `SelectOp` enum + pure apply fn; box/marquee select
(loose default, enclosed toggle); click-cycling through overlapping fixtures; select-passthrough
(grab a multi-selection without collapsing it); select-similar predicates (same GDTF/mode/
universe/group/model); hover highlight (reuse `pick`); deferred dirty-flag outliner↔viewport
reconcile + scroll-into-view; select-all action enum (All/None/Invert); lasso/circle paint
(defer); many-selection safety via robust single-step undo.

---

## 3. Prioritized backlog (all domains)

Sorted P0→P3, then by leverage. **Effort** S/M/L. **Seam** = target file/symbol in our code.
Every row is a real `apply_to_previz` item from the analyses.

| # | Pri | Eff | Domain | Feature | Seam |
|---|-----|-----|--------|---------|------|
| 1 | P0 | M | camera | Smooth/animated transitions for canned-view/frame/bookmark jumps (eased lerp, slerp yaw, persp↔ortho blend) | `camera.rs` OrbitCamera + `mod.rs:1753` frame_bounds / Action::View |
| 2 | P0 | S | camera | Zoom-to-cursor (dolly toward point under mouse; raycast anchor) | `camera.rs:198` zoom(), call site `panels.rs` scroll |
| 3 | P0 | S | camera | Frame-selected aspect correction (radius×aspect when wide) | `camera.rs:163`/`:189` frame/frame_aabb |
| 4 | P0 | M | gizmo | Grid/increment snap toggle + per-axis `step*round(v/step)` applier (Ctrl mid-drag toggle) | `panels.rs:1941` apply_transform; header |
| 5 | P0 | M | gizmo | Pivot-point selector (Median / 3D-Cursor / Individual-Origins / Active) | `mod.rs:207` TransformOp.pivot; `panels.rs:1941` |
| 6 | P0 | M | properties | Per-property reset-to-default arrow (default = GDTF/library template), shown only when differs | `panels.rs` fixture/env inspectors |
| 7 | P0 | M | properties | True multi-edit: mixed-value placeholder, write only touched fields | `panels.rs:760` bulk_inspector |
| 8 | P0 | M | properties | Simple/Advanced split inside each category | `panels.rs` fixture_inspector / environment_inspector |
| 9 | P0 | L | modes-menus | Named-registry menu/toolbar + unified Command (one source → keymap/menu/toolbar/palette) | merge `shortcuts.rs` Kmi + `op.rs` CATALOG; `mod.rs` menus |
| 10 | P0 | M | modes-menus | Declarative `ToolDef` table replacing ActiveTool match arms | `tools.rs:20` ActiveTool |
| 11 | P0 | S | modes-menus | Per-tool fallback op (click-selects under any tool) as a data field | `tools.rs` |
| 12 | P0 | M | undo | Delta-compress DocSnapshot (`Arc<[u8]>` + per-blob hash, share unchanged) | `op.rs:40` DocSnapshot |
| 13 | P0 | M | undo | Interactive begin→preview→finalize path for gizmo + slider drags (one step/gesture) | `op.rs` + `mod.rs:403` transform_before |
| 14 | P0 | M | input | Bind keymap items to op `id` (+props), not the closed `Action` enum | `shortcuts.rs:25` Action; `op.rs` dispatch |
| 15 | P0 | M | input | Diff-based user keymap persistence (store only overrides; recombine on load) | `shortcuts.rs`; prefs file |
| 16 | P0 | M | theming | Semantic token `Palette` (enum-indexed, live runtime table) | `theme.rs` apply(); literals in `panels.rs`/`tree.rs`/`editor.rs` |
| 17 | P0 | S | theming | Derive hover/press/disabled WidgetVisuals procedurally from one base (HSL) | `theme.rs` |
| 18 | P0 | S | theming | Single continuous `ui_scale` density pref (+ pixelsize hairlines) | `theme.rs`; `windows/preferences.rs` |
| 19 | P0 | S | content | Place at cursor/camera instead of origin; wrap add+place in one op | `panels.rs:432` add_library_row; `mod.rs:1855` AddAction |
| 20 | P0 | M | content | Recent + Favourites pseudo-categories in Library & Add menu (front-insert/de-dupe/cap/persist) | `panels.rs:443` library_browser; `add_menu.rs` |
| 21 | P0 | M | status | Handle-based status-bar message stack + grey hint slot | `mod.rs:2181` status_bar |
| 22 | P0 | M | status | Transient toast system with severity + action + permanent log history | new module; wire save/load/import/DMX/undo |
| 23 | P0 | S | status | Structured modal-hint HUD pill (read live keys from keymap) | `mod.rs:240` TransformOp::hint; `shortcuts.rs:414` poll_modal |
| 24 | P0 | M | selection | Refactor click+box onto one `SelectOp` enum + pure apply fn | `panels.rs` apply_fixture_click + inline toggle arms `:2671` |
| 25 | P0 | M | selection | Box/marquee select (screen-projected bounds, loose default) | `panels.rs`; `camera.rs:250` ray |
| 26 | P1 | M | outliner | Selection-aware + Shift-recursive eye toggle | `tree.rs` draw_row eye handler |
| 27 | P1 | S | outliner | Transparent-until-hover eye + quiet right gutter | `tree.rs` draw_row eye block |
| 28 | P1 | L | outliner | User folders as path strings + 3-band drag-reparent + cycle guard + validation text | `tree.rs` (TODO); `scene/mod.rs` add `folder:String` + folder_colors |
| 29 | P1 | M | outliner | Pluggable restriction columns (lock / select-disable), header-toggled | `tree.rs`; entity `locked:bool`; prefs |
| 30 | P1 | S | outliner | Isolate / solo shortcut (hide all but selection; reversible; Shift=extend) | `shortcuts.rs`; `tree.rs`/scene vis |
| 31 | P1 | M | properties | Inspector search matching labels+tooltips+enum options; auto-expand matches, dim rest | `panels.rs` inspectors; reuse `search` |
| 32 | P1 | S | properties | Semantic category ordering (Transform first, deterministic) | `panels.rs` inspector dispatch |
| 33 | P1 | M | properties | Right-click property menu: Copy / Paste / Reset to default | `panels.rs`; undo op |
| 34 | P1 | M | camera | Numbered camera bookmarks (Ctrl+n set / n jump, animated), persisted in `.archie` | `shortcuts.rs` Action::SetBookmark/Jump; `mod.rs`; scene/session |
| 35 | P1 | L | camera | On-screen nav gizmo cluster (axis-ball + pan/zoom/persp-ortho/frame) | `panels.rs` viewport overlay; `camera.rs` project_to_screen |
| 36 | P1 | L | camera | Quad/split layouts (plan+section+side+3D); per-pane camera+projection | `editor.rs`; `mod.rs` dock; camera off single shared OrbitCamera |
| 37 | P1 | M | gizmo | Orientation selector (Global / Local / View); Axis::vec → basis-column lookup | `mod.rs` TransformOp basis; `gizmo.rs` |
| 38 | P1 | S | gizmo | Expression + unit numeric entry ("1m","45deg","1.2*2") | `mod.rs:188` NumInput.value() |
| 39 | P1 | M | gizmo | Plane-constraint handles + double-tap-axis plane lock | `gizmo.rs` Handle enum; modal |
| 40 | P1 | M | gizmo | Ray-plane absolute drag (handle sticks to cursor; grab-offset) | `panels.rs:1962` move drag |
| 41 | P1 | L | modes-menus | Modes as registered objects (Layout/Patch/Focus/Render) + per-mode tool rail + remembered last tool | `editor.rs`; `tools.rs`; mode registry |
| 42 | P1 | M | modes-menus | Per-mode tool palettes in T-panel (named palette tabs) | `editor.rs`/T-panel; `tools.rs` |
| 43 | P1 | M | modes-menus | Dynamic/context menus built from selection (right-click + Shift+A) | `panels.rs`/`add_menu.rs` context menu |
| 44 | P1 | S | undo | RAII `Transaction` guard (snapshot before on construct, push on drop, .cancel/.group) | `op.rs`; `mod.rs` Ui::show commit sites |
| 45 | P1 | S | undo | Generalize coalescing: `undo_group` string; same-group consecutive pushes replace tip | `op.rs:168` amend_after → push() |
| 46 | P1 | M | undo | F3 command palette: poll-filtered fuzzy CATALOG list, each row shows bound shortcut | `op.rs` CATALOG; `shortcuts.rs:62` OpSearch |
| 47 | P1 | M | input | Keymap remapping UI (contexts→binds, click-to-capture, per-row + per-section Reset) | new Preferences tab; `shortcuts.rs` |
| 48 | P1 | M | input | Live conflict detection on rebind (masking-relationship classify) | `shortcuts.rs` gather(); reuse no_duplicate_binds test |
| 49 | P1 | M | input | Two shipped presets: previz-native (G/R/S) + Industry-Compatible (W/E/R, LMB-select, RMB-menu) | `shortcuts.rs` base tables |
| 50 | P1 | M | theming | Serializable dock layout persisted in prefs + `.archie` | egui_dock DockState; `mod.rs` dock; project format |
| 51 | P1 | M | theming | Tab-spawner registry that auto-builds the Window menu | `mod.rs` Tab::ALL/TOGGLEABLE/title/icon |
| 52 | P1 | M | theming | Two-axis theming: shared widget palette + per-editor canvas overrides (axis colors match gizmo.rs) | `theme.rs`; per-Tab override |
| 53 | P1 | M | content | Fuzzy + recency-weighted filter scorer (shared by Library + Shift+A) | `panels.rs:527`; `add_menu.rs:103` |
| 54 | P1 | M | content | Filter pills for fixture type/flags (PassesFilter closures, ANDed, persisted) | `panels.rs:512` LibSort area |
| 55 | P1 | L | content | Drag fixture from Library into viewport (ground ghost + drop), reuse place-at-cursor | `panels.rs:647` library_row_widget; viewport drop |
| 56 | P1 | M | status | Provider-keyed actionable warnings (in-viewport, fix button) | new module; viewport overlay |
| 57 | P1 | M | status | Async progress slot (done/total, ETA, cancel X) | `mod.rs:2181`; `share.rs` worker; project/MVR/3DS load |
| 58 | P1 | S | selection | Click-cycling through overlapping fixtures (ring after active) | `panels.rs:3688` pick()/apply_fixture_click |
| 59 | P1 | M | selection | Select-passthrough (keep multi-selection on click of a selected item; collapse on mouse-up if no drag) | `panels.rs` click handler |
| 60 | P1 | M | selection | Select-similar predicates (same GDTF/mode/universe/group/model); right-click + keymap | `panels.rs` context menu; over `Vec<Fixture>` |
| 61 | P2 | S | outliner | Search keeps folder path + highlights matched substring (fnmatch-style globbing optional) | `tree.rs` filter |
| 62 | P2 | M | outliner | Type/state filter chips (Fixtures/Objects/Screens; Unpatched/Selected/Conflicts) | `tree.rs` predicates over visible_* lists |
| 63 | P2 | S | outliner | Folder color tags (tint folder icon + hierarchy line) | `tree.rs`; Scene.folder_colors |
| 64 | P2 | S | properties | "Show only modified" filter toggle (reuse differs-from-default) | `panels.rs` inspector filter |
| 65 | P2 | S | properties | Pin inspector to a fixture (target independent of selection) | `panels.rs` inspector header |
| 66 | P2 | M | properties | EditCondition-style conditional gray-out (gray Beam angle when Beam=0) | `panels.rs` gdtf_inspector dynamic rows |
| 67 | P2 | M | camera | Walk mode (WASD+QE, eye height, optional gravity) for sightlines | `tools.rs`; camera fly-state |
| 68 | P2 | S | camera | Letter-key axis bindings + press-again-for-opposite | `shortcuts.rs` VIEWPORT keymap; `camera.rs` set_view |
| 69 | P2 | S | camera | Distance-scaled navigation (shared speed helper for pan/zoom/fly) | `camera.rs` pan/zoom |
| 70 | P2 | S | gizmo | Snap-target preview marker (draw snapped destination pre-release) | `panels.rs` viewport draw |
| 71 | P2 | L | gizmo | Snap fixture to scene vertex / truss node (nearest-vertex query) | `panels.rs`; spatial query over scene.geometry |
| 72 | P2 | S | gizmo | Screen-axis rotate ring + view-plane move centre | `gizmo.rs` Handle |
| 73 | P2 | M | modes-menus | Library as category/item placement registry (drag-to-patch-and-place) | `panels.rs` Library; registry |
| 74 | P2 | S | modes-menus | Tool-init gizmo flag (tools declare their own viewport gizmo group) | `gizmo.rs` for_tool → registry |
| 75 | P2 | M | undo | Inline Adjust-Last (F9): pop last step, re-exec with edited params | `shortcuts.rs:64` AdjustLast; LastOp; OpInvoke::Dialog |
| 76 | P2 | S | undo | Nested-op undo-depth guard (composite op pushes exactly one step) | `op.rs` UndoStack; run_op |
| 77 | P2 | M | undo | Unify Kmi + CatalogOp into one command descriptor (default+secondary chord, can_execute) | `shortcuts.rs` + `op.rs` |
| 78 | P2 | S | input | Dual chord slots per command (fold "(alias)" duplicate rows) | `shortcuts.rs` GLOBAL |
| 79 | P2 | S | input | Promote MODAL transform keys to the same editable table | `shortcuts.rs` MODAL/ModalAction |
| 80 | P2 | M | input | Project/studio keymap tier (app-default < show/studio < user) | `shortcuts.rs`; show file/template |
| 81 | P2 | L | theming | Collapsible edge sidebars / drawers (small-screen density) | custom egui overlay region |
| 82 | P2 | M | theming | User-savable named workspaces (not 3 hardcoded fns) | builds on serializable layout; prefs |
| 83 | P2 | S | status | Configurable status digest: memory/VRAM/version + selected/total counts | `mod.rs:2181`; `windows/preferences.rs` |
| 84 | P2 | M | status | Context-sensitive cursor hint on status-bar left | egui frame hovered-region; hint table |
| 85 | P2 | L | content | List ↔ Grid (tile) view with lazy thumbnails + remembered size | `panels.rs:647`; offscreen preview |
| 86 | P2 | M | content | Manufacturer/category catalog tree filtering the list by subtree | `panels.rs:573`; reuse `tree.rs` |
| 87 | P2 | M | selection | Deferred dirty-flag outliner↔viewport reconcile + scroll-into-view | one Selection; per-frame reconcile pass |
| 88 | P2 | S | selection | Select-all action enum (All / None / Invert) → A / Alt-A / Ctrl-I | `shortcuts.rs` |
| 89 | P3 | S | outliner | Solid-vs-dashed hierarchy lines (folder membership vs sub-geometry parenting) | `tree.rs` line paint |
| 90 | P3 | M | outliner | Drag-paint visibility sweep down the eye column | `tree.rs`; drag-state over virtualized rows |
| 91 | P3 | L | properties | Favorites: star fields into a pinned section; persist per fixture type | `panels.rs`; project/workspace config |
| 92 | P3 | L | properties | Section chips above inspector (Position/Color/Beam/DMX cross-cutting axis) | `panels.rs` |
| 93 | P3 | S | camera | Center-pick orbit pivot + FOV/clip DragValues in N-panel | `panels.rs`; `camera.rs` fields |
| 94 | P3 | S | gizmo | Per-type snap increments (0.25m / 15° / 10%) | snap config |
| 95 | P3 | S | modes-menus | Event-driven active-tool/menu refresh (recompute on context change) | mode→tool resolution |
| 96 | P3 | S | undo | Held-key repeat guard on destructive/coalescing shortcuts | `shortcuts.rs` dispatch; per-Action repeat flag |
| 97 | P3 | S | undo | Formalize stable-reference revalidation hook on restore (cues/groups→fixtures by id) | `op.rs` restore (ensure_ids + gdtf reattach) |
| 98 | P3 | M | input | Extended trigger vocab (Press/Release/Click/DblClick/Drag) + repeat-ignore | `shortcuts.rs` Event::*; fired()/dispatch |
| 99 | P3 | S | input | Searchable remap list + show-shortcut-in-menus (one source of truth) | reuse F3 infra; `op.rs` CATALOG |
| 100 | P3 | S | theming | Style bundles for bespoke widgets (dock tab, tool rail, outliner row) | `tree.rs`/`editor.rs`/`tools.rs` |
| 101 | P3 | S | theming | Per-widget roundness/gradient tokens | `theme.rs` |
| 102 | P3 | L | content | Saved fixture "kits" (library-side collections) | `panels.rs:342` LibState.selected; project |
| 103 | P3 | M | content | key:value search terms ("maker:robe","beam>20") | fuzzy filter layer |
| 104 | P3 | S | status | Cursor-as-progress for fast blocking ops | egui pointer icon |
| 105 | P3 | S | status | Named realtime-override reason badge (active cue / DMX live / recording) | viewport corner |
| 106 | P3 | L | selection | Lasso / circle paint select (continuous-add brush) | `panels.rs`; point-in-poly |
| 107 | P3 | S | selection | Many-selection safety via robust single-step undo of selection changes | selection undo path |

---

## 4. Skip list — patterns we deliberately won't port

Reasons grounded in the analyses. These are game-only / animation-only / DCC-only or pipeline
concerns with no previz analogue.

**Outliner / data model**
- UE Source-Control + Unsaved/external-actors columns; World Partition / `ActorDesc` streaming
  items — tied to per-actor-file + Perforce world workflows; `.archie` is one bundled file.
- UE Component sub-trees + Blender per-ID datablock sub-trees (mesh/material/modifier/constraint/
  bone rows) — DCC datablock browsing; our outliner stays at entity granularity.
- Blender Data-API / Libraries / Orphans / Library-Override display modes — linked-library + RNA
  introspection; no analogue in a self-contained file. (Keep only the per-frame-view idea.)
- Blender holdout / indirect-only render columns; animation items (bone collections, pose, NLA,
  drivers, grease-pencil) — render-layer compositing + skeletal animation, neither exists here.
- UE Picking/Folder-Picking outliner *modes* — modal embedded pickers; ours is always browsing.

**Properties**
- Blender's 14+ horizontal context-tab strip — most tabs are game/DCC/animation; our coarse
  per-type dispatch is the right granularity.
- Keyable/animated property filters + per-row keyframe decorate dot — animation tracks; cues are
  crossfades, not per-property keyframes.
- EditInlineNew / instanced-subobject reflective trees; RNA-reflection auto-layout;
  PropertyRowGenerator / async detail-view diffing; DetailsViewStyle config-ini machinery —
  reflection/diff tooling outside previz scope; our hand-written egui inspectors suffice.

**Camera / nav**
- Blender FLY 6-DOF + gravity/jump/teleport; NDOF/SpaceMouse; per-axis 4-way view-roll + free
  horizon-tilt roll — game ergonomics / niche hardware; arch-viz wants level horizons + a
  grounded walk.
- UE's huge view-MODE matrix (ShaderComplexity, Nanite/Lumen/VSM viz, LightmapDensity, LOD/HLOD,
  collision, GPU skin cache) — engine rendering debug; our ViewportMode set is the right scope.
- UE full actor-PILOT possession + per-focus ortho auto-clip-to-selection — port only "look
  through a fixture/show-camera"; dynamic clipping would pop volumetrics.

**Gizmos / transform / snap**
- All edit-mode transform converters (mesh/curve/lattice/armature/grease-pencil/uv/particle/
  sculpt/mask) + animation/graph/NLA/sequencer transforms — element-level / timeline; previz
  transforms whole entities.
- Edge/vertex slide, bend, shrink-fatten, bone-roll, trackball free-rotate; full surface/edge/
  volume snap-object pipeline; UE InteractiveTools GizmoElement* component zoo — modeling/rigging
  internals; a simple vertex/node snap covers 90% of placement; we draw handles with egui.

**Modes / menus**
- Brush/paint tool plumbing; multi-space-type tool keying; UE's heavy `UEdMode`/`UToolkit`
  recycled-mode lifecycle; Blueprint/Python menu scriptability; MultiBox customization profiles;
  Foliage/Landscape/MeshPaint editor modes — sculpt/level-design/runtime-scripting, none apply.

**Undo / commands**
- UE `UObject::Modify()` reflective per-object serialization (forget-it = silent no-undo
  footgun); Blender's full pluggable per-domain UndoType vtable (exists for incompatible edit
  modes we don't have); mesh/sculpt accumulating undo; transaction↔package-dirty/SavePackage/PIE
  coupling; Verse VM transactions; OPTYPE_MACRO + RNA property reflection; multi-context input
  binding-manager stack.

**Input / keymap**
- Pie menus / radial gesture UI; tablet/stylus/NDOF events; per-addon keymap layer (no plugin
  system); RNA/IDProperty operator-property editing in the keymap editor; mouse-emulation
  permutation-matrix generator; UE RadioButton/Check toggle-state taxonomy on commands.

**Theming / docking**
- Blender's vertex/edge screen graph + in-place editor-type swap with per-space data stacks; UE
  NomadTab cross-window tear-off + LayoutExtender plugin injection; the huge per-domain
  ThemeSpace fields (graph/NLA/sequencer/node) + bone-color sets; UE string-keyed brush registry
  as the *primary* API (use the typed enum table instead).

**Content library**
- Source-control frontend filters; datablock link/append machinery (we bundle bytes); actor-
  factory class-spawning registry; thumbnail edit-mode dirty/checkout badges; collection
  source-control sync / dynamic-query engine; Blender generic remote-library jobs (we have
  `share.rs`); add-mesh-primitive operators.

**Status / overlays**
- UE status-bar drawers (content-browser/output-log slide-ups); SourceControl menu + onboarding
  survey; Blender's full hierarchical `stat <group>` profiler HUD (our gpu_timer/perf_overlay
  is the right slice); PIE overlay chrome; SafeFrames broadcast guides; AsyncTaskNotification
  prompt-and-block variant.

**Selection**
- GPU select-id buffer / hit-proxy infra (CPU ray-pick + screen-projected bounds suffices); all
  edit-mode element + pose-bone + weight/vertex-paint + BSP/brush surface selection; object-
  interaction-mode locking + mode-switch-on-select (our app Modes are user-chosen, not implicit);
  Blueprint-component drill-down beyond one fixture→cell level; PIE / source-control-aware
  gating; keying-set / render-pass / hook select-grouped predicates.

---

## 5. How this layers onto `RESEARCH-blender-framework.md`

The framework doc is the *architectural skeleton* (operator/undo/keymap/editor/tools/datablock/
modes pillars + a 5-phase roadmap). This doc is the *feature/UX flesh* mined from concrete
Blender+UE behaviour. They overlap deliberately; the mapping below prevents duplicate work.

- **Framework Phase 1 (operator + undo + keymap-v2)** is the home for backlog #12 (delta-compress
  DocSnapshot), #13 (interactive transaction path), #14 (bind to op `id`), #15 (diff-based keymap
  persistence), #44 (RAII Transaction guard), #45 (coalescing by `undo_group`), #76 (nested-op
  depth guard), #97 (stable-ref revalidation), and #77 (unify Kmi+CatalogOp). These are exactly
  the "scalability core" the framework doc flags as unblocking everything — treat §3's items as
  the *concrete task list* for that phase, not new scope.

- **Framework Phase 2 (editor/header + N/T panels)** already shipped per recent commits;
  remaining overlap is #36 (quad/split layouts) and #93 (N-panel FOV/clip), which extend the
  per-editor chrome.

- **Framework Phase 3 (tools + gizmos)** already shipped (tool system + G/R/S + gizmos).
  This doc's gizmo backlog (#4 snap, #5 pivot, #37 orientation, #38 numeric, #39 plane, #40 ray-
  plane drag, #70–72) is the *deepening* pass on that pillar; #10/#11 (ToolDef table + fallback
  op) refactor the existing tool model into the declarative form Phase 3 envisioned.

- **Framework Phase 4 (datablock / outliner / properties)** is where the bulk of §2.1 (outliner
  #26–30, #61–63, #89–90) and §2.2 (properties #6–8, #31–33, #64–66, #91–92) land. The framework
  doc calls out "benefits from Phase 1's IDs-in-snapshots" — #28 (folders as path strings) and
  the pluggable-column work depend on the Phase-1 snapshot/undo plumbing being solid first.

- **Framework Phase 5 (pie / modes / full keymap — the capstone)** absorbs #41–42 (registered
  modes + per-mode palettes), #47–49 (remap UI + conflict detection + presets), #78–80 (chord
  slots / modal-keys-as-data / studio tier), #98 (trigger vocab). The framework doc lists pie
  menus here; this doc *declines* pie menus (§4) and substitutes the named-registry menu/toolbar
  system (#9) + F3 palette (#46) as the better fit for lighting users — note this divergence so
  Phase 5 doesn't build the pie UI.

- **Not in the framework doc at all (net-new tracks this doc adds):** the entire status/
  notifications/overlays domain (#21–23, #56–57, #83–84, #104–105 — we have no toast/report
  system today), the theming token model (#16–18, #50–52, #81–82, #100–101), the content-library
  placement/search/recent work (#19–20, #53–55, #73, #85–86, #102–103), the camera-navigation
  polish (#1–3, #34–35, #67–69), and the selection-systems overhaul (#24–25, #58–60, #87–88,
  #106–107). These can proceed in parallel with the framework phases since they sit above the
  op/undo/keymap core rather than redefining it — though #19/#22/#34/#50 should consume the
  Phase-1 Transaction guard and persistence paths once they exist.

**One-line rule of thumb:** the framework doc says *what the architecture is*; this doc says
*which behaviours to build on it and in what order*. Where they name the same thing (undo,
keymap, tools, outliner, properties, modes), this doc's §3 rows are the implementable backlog —
do not re-spec them in the framework phases.
