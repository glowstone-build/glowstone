# The Blender Framework, rebuilt as an arch-viz / lighting-previz tool

This is the architecture blueprint for turning previz into something that *feels*
like Blender — its proven, scalable operating model — while staying a
purpose-built arch-viz / lighting-previz application. It synthesizes six pillar
specs (operator+undo, area/region/editor, keymap+events, tools+gizmos,
datablock/outliner/properties, pie/modes/workspaces) into one coherent plan.

It **builds on** [`docs/RESEARCH-ux.md`](RESEARCH-ux.md) — personas (Robin LD /
Sam patch / Alex visualiser), the **Patch → Position → Focus → Colour → Look →
Visualise** workflow, the dock-layout rationale, the existing shortcuts and the
three workspaces. Read that first. This doc does not repeat persona/workflow
reasoning; it defines the *machinery* underneath the UX that doc already argues
for, and extends the workflow into a formal **mode** model.

---

## 1. Vision — "Blender's operating model, rebuilt as an arch-viz tool"

Blender scales from a first-time user to a TD doing thousand-object scenes on the
*same* interface because of six interlocking design choices. We adopt the
**choices**, not the code. The philosophy, stated as invariants this codebase
should converge on:

1. **Keyboard-first, eyes-in-the-viewport.** Every frequent action has a key; the
   hand stays on the keyboard and the eyes stay on the picture. (RESEARCH-ux.md
   already commits to this for Robin; we make it structural, not ad-hoc.)
2. **Everything the user does is an *operator*.** Add, delete, patch, move,
   aim, set-HDRI, edit-cue — each is a named, dispatchable unit with a stable id,
   a label, a poll (can it run now?), and an exec. The keymap, the menus, the
   command palette and the redo panel all dispatch the *same* operator vocabulary.
3. **Editing is undoable; the *system* pushes undo, not the operator.** A user
   should never fear an edit. Undo is a property of "did the document change",
   decided once, centrally, after an operator finishes.
4. **Modal, non-blocking interaction.** G/R/S are spring-loaded modal operators
   today; that lifecycle (invoke → live modal → confirm/cancel) becomes the
   *general* shape of interactive editing, never a blocking dialog.
5. **Context decides meaning.** The same key does different things in different
   *editors* and *modes*; the same panel can become a different editor in place.
   Specific context beats general — and that resolution is data, not branch code.
6. **Live.** Incoming DMX, cue playback, HDRI lighting all keep running while you
   edit. The undo/authoring layer must never fight the live-driven layer.

The target feel: a Robin/Sam/Alex sits down, muscle-memory from Blender mostly
works, but every surface is about *light, fixtures, addresses and the rendered
room* — not meshes, sculpt brushes or grease pencil.

---

## 2. The framework pillars

Each pillar: **Blender's model → our reimplementation → the integration seam in
our code.** All designs are clean-room (§5): we restate behaviour in our own Rust
types; no GPL source is copied.

### 2.1 Operator + Undo/Redo — the scalability core

**Blender's model.** `wmOperatorType` is an immutable blueprint: an idname
(`"object.delete"`), a label, a property set, flags (`OPTYPE_REGISTER`,
`OPTYPE_UNDO`, `OPTYPE_INTERNAL`, `OPTYPE_UNDO_GROUPED`) and callbacks
(`poll`/`exec`/`invoke`/`modal`/`cancel`). Callbacks return a status bitflag
(`FINISHED`/`CANCELLED`/`RUNNING_MODAL`/`PASS_THROUGH`). The *window manager*, not
the operator, performs the undo push — only on `FINISHED` when `OPTYPE_UNDO` is
set. The undo system is a doubly-linked `UndoStack` of `UndoStep`s; "global undo"
serializes the whole datamodel per step (cheaply, via implicit-sharing). F9
"Adjust Last Operation" = undo + re-exec with edited props; F3 = flat search over
the registry; Shift-R = repeat-last.

**Our reimplementation.** New module `src/ui/op.rs`.

```rust
// Snapshot of the *authored document only* (not assets, not live-DMX values).
struct DocSnapshot { scene: Vec<u8>, patch: Vec<u8>, cues: Vec<u8>,
                     groups: Vec<u8>, selection: Selection }   // Vec<u8> = bincode
fn capture(scene,&patch,&cues,&groups,&selection) -> DocSnapshot;
fn restore(&self, &mut scene, &mut patch, &mut cues, &mut groups, &mut selection);

struct UndoStep  { name: String, before: DocSnapshot, after: DocSnapshot }
struct UndoStack { steps: Vec<UndoStep>, cursor: usize,
                   limit_steps: usize /*64*/, limit_bytes: usize /*~256MB*/,
                   last_op: Option<LastOp> }
// begin()->before ; push(name, before, doc..) ; undo(doc..) ; redo(doc..)

trait Operator {
    fn id(&self) -> &'static str;          // "object.delete"
    fn label(&self) -> &'static str;       // "Delete"
    fn flags(&self) -> OpFlags;            // REGISTER | UNDO | INTERNAL
    fn poll(&self, cx: &OpCtx) -> bool;
    fn exec(&mut self, cx: &mut OpCtx) -> OpStatus; // Finished/Cancelled/RunningModal/PassThrough
}
struct OpCtx<'a> { scene:&mut Scene, patch:&mut PatchTable, cues:&mut CueEngine,
                   groups:&mut Vec<SelectionGroup>, selection:&mut Selection,
                   library:&Library }
```

Central dispatch on `Ui::run_op`: `if !poll {return}; let before = stack.begin();
match exec(cx) { Finished => { if UNDO {stack.push(label, before, ..)} if REGISTER
{last_op = ..} } Cancelled => {/*no push*/} .. }`. This is Blender's "system pushes
undo after Finished" rule. We store **both** ends of each step (`before`,`after`)
so undo/redo are symmetric — simpler and correct, no decode-direction subtlety.
Modal operators reuse the existing `TransformOp` shape: invoke captures `before`,
modal re-applies live (no push), confirm pushes, Esc restores `before`.

We deliberately use **full-document bincode snapshot** as our analogue of
Blender's memfile undo — the authored doc (fixtures/patch/cues/groups, *not* the
GDTF/model/HDRI asset blobs) is tiny, so we skip implicit-sharing entirely
(revisit only if perf bites). `DocSnapshot` is shaped so it *could* become
field-granular later.

**Seam.** `src/ui/shortcuts.rs:Action` (line 21) is already the proto-operator
registry; `BINDINGS` (line 143) already pairs Action+label+category = the F3/redo
metadata. The dispatch site is `Ui::handle_shortcuts` (mod.rs:1104) and the
post-`DockArea` "deferred commit" point in `Ui::show` — the place where
`scene`/`patch_mut`/`cues`/`groups` are all reachable (already used for
`commit_delete` / patch dialog). `commit_delete` (mod.rs:1216) is literally the
body of `object.delete`'s exec; precede it with a snapshot push. `project.rs`
already proves all these fields round-trip through bincode. The `UndoStack` lives
on `Ui` (next to cues/groups), travels with the document, and is **not**
serialized into `.archie`.

### 2.2 Area / Region / Editor / Header + N/T panels

**Blender's model.** `wmWindow → bScreen → ScrArea → ARegion`. A `ScrArea` is one
rectangular *editor* with a `spacetype` (View3D/Outliner/Properties/…). Each editor
owns regions: `RGN_TYPE_HEADER` (the per-editor bar; first widget is always the
editor-type switcher), `RGN_TYPE_WINDOW` (main content), `RGN_TYPE_UI` (the N-panel
/ sidebar, toggled by **N**), `RGN_TYPE_TOOLS` (the T-panel toolbar, toggled by
**T**). Areas split/join via a vert/edge graph; switching editor type swaps the
spacetype in place, keeping geometry.

**Our reimplementation.** **egui_dock already gives us the hard part** — the
Area split/join/drag/maximize graph (and it's MIT/Apache, so this is the big
license-safe win; we never touch Blender's `ScrVert`/`area_split`). What we add is
the **header** and the **N/T regions**, carved from each leaf's `ui` via
`egui::TopBottomPanel::show_inside` / `SidePanel::show_inside` (verified available
in egui 0.34; egui_dock 0.19's `TabViewer` has no header hook, so we draw it
ourselves inside `PanelViewer::ui`).

New module `src/ui/editor.rs`:

```rust
enum RegionKind { Header, Main, NPanel, TPanel }  // clarity
fn editor_header(viewer, ui, tab: &mut Tab)        // TopBottomPanel::top(26px-ish).show_inside
// first widget = editor_type_switcher(ui, tab): menu over Tab::ALL, writes *tab in place
struct ViewportRegions { n_panel_open: bool, t_panel_open: bool, n_tab: NTab } // on Ui

// later (generalisation):
trait Editor {
    fn header(&mut self, ui, cx);
    fn main(&mut self, ui, cx);
    fn n_panel(&mut self) -> Option<NPanelFn>;   // viewport reuses panels::inspector verbatim
    fn t_panel(&mut self) -> Option<ToolRailFn>; // the tool rail (§2.4)
}
```

The Viewport header absorbs today's floating display overlay (Mode + Exposure,
panels.rs ~3013) and the View-menu toggles + N/T toggle buttons. The Viewport
N-panel renders **`panels::inspector` unchanged** (Item/Transform where the eyes
are — exactly RESEARCH-ux.md's "control belongs where the eyes are" rationale).
The T-panel is the tool rail (§2.4). Because `Tab` derives Serialize and lives in
`DockState<Tab>`, swapping a leaf's editor type persists for free.

**Seam.** `PanelViewer::ui` (mod.rs:1801) is THE seam — carve header + side
regions from `ui` *before* dispatching main content. `Tab` (mod.rs:68) + `title`
+ `icon` is the SpaceType registry; add `Tab::ALL`. `panels::viewport`
(panels.rs:2393) and `panels::inspector` (panels.rs:1053) are reused as the main
and N-panel bodies. `default_dock`/`workspace_dock` (mod.rs:727/734) are unchanged
— they already build the splittable graph.

### 2.3 Keymap + Events — context-stacked, remappable

**Blender's model.** A `wmKeyConfig` preset (e.g. `blender_default`,
`industry_compatible`) holds keymaps tagged by space type + region + a `modal`
flag, each with a `poll` gate. A `wmKeyMapItem` is one binding: operator idname +
**properties** (same op, different args = different bind), event `type`,
`val` (PRESS/RELEASE/CLICK/DBL/CLICK_DRAG), **tri-state** modifiers
(on/off/`KM_ANY`). User edits are stored as **diffs over defaults** so they
survive default changes. Dispatch walks a **most-specific-first** handler stack;
first match wins. A **modal keymap** maps events to named actions
(CONFIRM/CANCEL/AXIS_X/…) the running modal op interprets.

**Our reimplementation.** Evolve `shortcuts.rs` into `src/ui/keymap/` (mod / defaults
/ event / editor), with a `pub use keymap::*` shim during migration.

```rust
struct KeyEvent { trigger: Trigger, mods: Mods }
enum   Trigger  { Key(egui::Key), Pointer(egui::PointerButton) }
enum   Press    { Press, Release, Click, DoubleClick, ClickDrag }
struct Mods     { shift: Tri, command: Tri, alt: Tri }   // Tri = Off|On|Any (fixes exact-bool today)

struct Kmi    { event: KeyEvent, op: Op, press: Press, active: bool, id: KmiId }
struct KeyMap { id: KeymapId, items: Vec<Kmi>, modal: bool }
enum   KeymapId { Global, Editor(Tab), Mode(Mode), Modal(ModalMap) }

struct KeyConfig { preset: Preset, user_diffs: Vec<KmiDiff> } // serialize this only
enum   Preset    { Previz, IndustryCompatible }
fn resolved(&self) -> Vec<KeyMap>                            // apply diffs over defaults

enum ModalAction { Confirm, Cancel, AxisX, AxisY, AxisZ, PlaneX, Precision, Snap, .. }
fn poll_modal(ctx, ModalMap) -> Vec<ModalAction>            // replaces raw key reads in viewport()

struct Dispatcher { config: KeyConfig }
fn poll(&self, ctx, stack: &[KeymapId]) -> Vec<Op>          // most-specific-first, Tri::Any wildcard
```

The dispatcher builds the stack `[Editor(active_tab), Mode(workspace), Global]`,
iterates in order, returns the first matching Kmi's `Op` — this **kills the
`s_is_scale`/`nudge_ok` gating hacks** (mod.rs ~1112) by making context membership
the gate. `Op` keeps today's `Action` variants (`View(CameraView)`,
`Nudge(Dir,f32)` are already idname+properties) plus `fn id()`/`fn label()`.
We build on **egui::Event** (not raw winit) inside `ctx.input()`, so there is one
input pipeline. Conflict detection is redefined as *same-event-within-one-keymap*
(the existing `no_duplicate_binds` test generalises per-keymap). A Preferences >
Keymap editor (preset switch, Name/Key search, inline capture, per-row restore,
conflict highlight) is the last phase; the cheat sheet
(`src/ui/windows/shortcuts.rs`) becomes its read-only view.

**Seam.** All of `shortcuts.rs` (Bind/Action/Context/BINDINGS/poll). Callers at
mod.rs:788/1107/1197 (Global), panels.rs:2549/2600/2865 (modal/transform/dup).
`Workspace` (mod.rs:208) and `Tab` (mod.rs:68) are the (mode, editor) axes;
`viewport_focused` (mod.rs:277) generalises to `active_editor: Tab`. Persistence
rides on `Preferences` (windows/preferences.rs — derives serde but is **not yet
persisted**; this pillar must be sequenced with general preference persistence or
it ships unsavable).

### 2.4 Tools + Gizmos — active-tool model + gizmo-group framework

**Blender's model.** The **active tool** (`bToolRef`, one per workspace+space+mode)
— not the selection — decides what a viewport press/drag does and which gizmo
group draws. The T-panel toolbar is a radio column generated from the registered
tools; tool options live in the header. A `wmGizmoGroupType` is a factory with
`poll`/`setup`/`refresh`/`draw_prepare`; a `wmGizmo` owns a `matrix_basis`,
colours, and a `highlight_part` (hovered sub-handle, -1 = none) with callbacks
`test_select`/`draw`/`invoke`/`modal`/`exit`. The xform gizmo group reads the
active tool to instantiate arrows (Move) / dials (Rotate) / boxes (Scale) at the
pivot, colours X=red/Y=green/Z=blue. **Select is the fallback**: a plain click
still selects while another tool is active.

**Our reimplementation.** New `src/ui/tools.rs`:

```rust
enum ActiveTool { Select, Move, Rotate, Scale, Transform, Aim, Measure, Add }
impl ActiveTool { fn icon/label/tooltip/shortcut(self); const TOOLBAR: [..];
                  fn shows_xform_gizmo(self) -> bool; }

trait GizmoGroup {
    fn poll(&self, cx: &GizmoCtx) -> bool;
    fn refresh(&mut self, cx: &GizmoCtx);
    fn test_select(&self, p: egui::Pos2, cx: &GizmoCtx) -> Option<Handle>;
    fn draw(&self, painter, cx, hover: Option<Handle>);
    fn invoke(&mut self, h: Handle, cx: &GizmoCtx) -> Option<TransformOp>;
}
enum Handle { Axis(Axis), Plane(Axis), View, AimTarget, MeasureEnd(u8) }
struct GizmoCtx<'a> { camera, scene:&Scene, selection:&Selection, rect, vp, pivot }
```

`XformGizmo` extracts today's screen-space move-handle code and extends it: arrows
(Move) → +dial rings (Rotate) → +scale boxes (Scale) → all (Transform). Its
`invoke` returns a `TransformOp{from_gizmo:true, kind, gizmo_hovered_axis}` —
**reusing the existing op pipeline verbatim**. `AimGizmo` (the lighting
differentiator: click a floor/geometry point → solve pan/tilt to point the
selected head there) and `MeasureGizmo` (two points + ruler + distance label
honouring `prefs.units_feet`, never mutates scene) are non-transform tools that
prove the trait. `Axis::color()` (mod.rs:148) stays the single colour source.
The tool rail is an `egui::SidePanel::left("toolbar").exact_width(36px)` reserved
in `show()` before the DockArea (or a per-viewport overlay — pick one early; §6).
**Critical ordering:** gizmo `test_select`/invoke must run *before* orbit/select
(as the current block does) or a handle-drag will orbit the camera. Spring-loaded
G/R/S keys stay unchanged and *additionally* set `active_tool` for visual
consistency. **Aim must route through the same pan/tilt target the cues/slew use**
(PositionMSpeed inverse, target-vs-actual — see the sim-UX memory), not a raw
quaternion poke, or it fights the motion engine.

**Seam.** `panels.rs:viewport` (2393) — the move-gizmo block (~2436-2541), modal
G/R/S (~2543-2653), orbit/pick/context-menu. `apply_transform` (panels.rs:2306)
is the math kernel Aim extends. `TransformOp`/`Axis` (mod.rs:103-204) reused.
`pick()`/`dist_point_segment`, `camera.ray`, `RenderSettings.axis_hint` reused for
Aim/Measure 3D lines.

### 2.5 Datablock / Outliner / Properties

**Blender's model.** Every serializable thing embeds an `ID` (name, library link,
user-count `us`, fake-user flag). `us==0` data is purged on save unless fake-user
pins it. A **library override** is a local proxy of a linked datablock that keeps
unmodified props tracking the source while edited props stay local. `Main` is one
list per datablock type. The scene graph: `Scene → Collection` (named container
holding objects + nested collections); `Object` (placement wrapper) *links* to its
ObData (Mesh/Light/Camera). The **Outliner** is a recursive tree with display
modes (View Layer / Blender File / Orphan Data) and restriction columns
(eye/selectable/render). The **Properties** editor is a vertical context tab-stack
(Render/Output/World/Scene/Object/Modifiers/Data/Material…) that auto-shows tabs
by the active object's type.

**Our reimplementation.**

- **Stable IDs (foundational).** `struct EntityId(u64)` / `AssetId(u64)` +
  per-scene `next_id`; `id` on `Fixture`/`SceneGeometry`/`LedScreen`/`Environment`;
  `Scene::index_of(id)`. Selection/groups/cues/collections reference **IDs**, not
  indices — this **kills the index-corruption-on-delete bug class** the memory
  documents, and makes undo snapshots robust. (Migration: assign ids sequentially
  on load of old `.archie`.)
- **Datablock + user counts (derived, no `us` field).** `trait DataBlock { fn
  name; fn kind; }`; `fn asset_users(scene) -> HashMap<AssetKey, usize>` counting
  Arc/by-name links (GDTF profile, HDRI, screen profile, model). `Library::orphans`
  = 0-user assets → a "Purge unused" action for the `.archie` bundle.
- **Shallow hierarchy.** `struct Collection { id, name, expanded, vis, children:
  Vec<Node> }`, `enum Node { Collection, Fixture(EntityId), Geometry, Screen,
  Environment }`, World implicit-root. Vecs stay storage-of-record; Nodes hold IDs.
  Migration drops existing instances into kind-collections (reproduces today's
  outliner exactly). Single-membership, 1–2 levels deep — we **skip the full DAG**.
- **Visibility bitset.** `struct Visibility { hidden, locked, render }` replaces
  the lone `SceneGeometry.hidden`, added to all instance kinds. Renderer reads
  `hidden`; pick skips `locked`; headless capture skips `!render`. Collections fold
  visibility onto descendants via `Scene::effective_vis(id)` (cache per scene-edit,
  not per frame, to protect the many-fixtures perf the volumetric work fought for).
- **Outliner modes.** `enum OutlinerMode { ViewLayer, ProjectFile, Orphan }` —
  ViewLayer walks `scene.root` with disclosure + eye/lock/render columns;
  ProjectFile lists assets by type with user-counts (the view onto the `.archie`
  bundle); Orphan lists 0-user assets + Purge.
- **Properties context tab-stack.** `enum PropTab { Render, World, Scene, Object,
  Fixture, Optics, Patch }`; `properties_panel` draws a thin Phosphor icon rail +
  the active page, **reusing the existing `*_inspector` fns verbatim** as page
  bodies (Object = transform + Visibility, the unifying page for any instance kind).
  Tab availability filters by selection kind (geometry → Object; fixture →
  Object/Fixture/Optics/Patch; nothing → Render/World/Scene); auto-snaps to a valid
  tab on selection change. `Tab::Inspector` → `Tab::Properties` (dock migration).

**Seam.** `Scene{fixtures,environments,world,geometry,screens,mvr}` (scene/mod.rs)
and `Selection` (scene/mod.rs:212) — World-as-top + `Selection.world` are already
the hierarchy seed; `scene_outliner` already nests Environment under World. The
single deletion path `commit_delete` (mod.rs:1216) + `remap_index` is where ID
references must flow (or, better, IDs make the remap unnecessary). `inspector`
(panels.rs:1053) already groups Transform/Fixture/Optics/Wheels collapsibles —
the latent PropTabs. **`.archie` is positional bincode** — adding `id`/`vis`/`root`
fields misaligns old saves; a **versioned migration in project.rs is mandatory**
(the single biggest correctness trap).

### 2.6 Pie menus / Modes / Workspaces

**Blender's model.** A **Workspace** bundles a layout (`bScreen`) + an active
**mode** + a per-mode active tool; the top tabs (Layout/Modeling/Sculpting/…) each
preset a layout *and* a mode. A **mode** (`object_mode`: Object/Edit/Sculpt/Pose/…)
reconfigures three things at once: the header+toolbar, the active **keymap**, and
what is selectable/editable (it gates which operators `poll` true). **Pie menus**
are radial, hold→flick→release-to-confirm gesture menus (Z=Shading, `=View,
Ctrl-Tab=Mode) designed for eyes-up muscle memory.

**Our reimplementation.**

- **`enum Mode { Layout, Patch, Focus, Render }`** — the arch-viz stages of
  RESEARCH-ux.md's Patch→Position→Focus→Colour→Look→Visualise. `fn editable(self)
  -> Editable` returns a bitset {fixtures_transform, geometry_transform, optics,
  patch, camera_world} consulted as the **single source of truth** by
  `viewport()`, `inspector()` and keymap gating. `fn default_inspector_category`.
- **Workspaces bind layout + mode.** Extend `Workspace` with `fn mode(self)`
  (Design→Layout, Patch→Patch, Look→Focus, Visualise→Render);
  `set_workspace(ws)` sets both `self.dock` and `self.mode`. Promote workspaces to
  visible top tabs (a thin `TopBottomPanel::top` of selectable labels above the
  dock).
- **Pie widget.** New `src/ui/pie.rs`: `struct PieMenu { center, items, opened_at,
  armed }`; `show(ui, key_still_held) -> Option<PieAction>` draws compass slices,
  highlights past a `DEAD_ZONE` threshold, confirms on key-release-past-threshold
  (flick) or slice-click (sticky), cancels on Esc/outside/right-click.
  `enum PieAction { Shading(ViewportMode), View(CameraView), Mode(Mode), Toggle }`.
  Three pies: Shading (Z), View (`), Mode (Tab).

**Seam.** `Workspace` (mod.rs:208) + `workspace_dock` (mod.rs:734) +
`menu_bar` Window>Workspace block. `ViewportMode` (scene/mod.rs, has ::ALL) and
`CameraView` (camera.rs, ::ALL + set_view) are the ready-made pie targets.
`Ui` gains `mode`, `pie`. **Gesture caveat:** pies need press-hold-release, but
`shortcuts::poll` is edge-based (`key_pressed`) — read `i.key_down()` for the
hold/confirm in `viewport()` while the press edge opens the pie; document this
divergence in the registry comment. Mind key collisions: Z is AxisLock during
modal, Tab is egui focus-traversal — pies must only fire when viewport is focused,
no modal transform is active, and no text field has focus.

---

## 3. Modes and Workspaces — concrete definitions

Four **modes**, each changing four things (editable target set / default inspector
category / which gizmos draw / live keymap context):

| Mode | Persona | Editable | Gizmos | Inspector default | Blender analogue |
|---|---|---|---|---|---|
| **Layout** | Robin/Sam | fixture + geometry **transforms**, add/delete | Move/Rotate/Scale xform gizmo | Transform | Object Mode |
| **Patch** | Sam | **addresses/universes/modes** only; transforms locked | none | Patch | data-edit modes |
| **Focus** | Robin | fixture **optics** (pan/tilt/colour/intensity/gobo); geometry locked | Aim gizmo | Fixture/Optics | Pose ("pose the rig") |
| **Render** | Alex | **camera + world/HDRI + exposure**; scene view-only | none | Render |

Four **workspaces** (top tabs), each = a dock layout + a default mode:

| Workspace | Layout (from RESEARCH-ux.md) | Default mode |
|---|---|---|
| **Design** | outliner+library left, inspector right, Fixtures/DMX strip | Layout |
| **Patch** | tall Fixtures+DMX data area, thin viewport | Patch |
| **Focus/Look** *(new)* | viewport-dominant + Inspector/Optics + Cues | Focus |
| **Visualise** | maximised viewport, thin Scene(World)+Inspector | Render |

Clicking a workspace tab swaps the egui_dock `DockState` **and** sets the mode.
Mode + active workspace must round-trip through `.archie`/autosave (§6 risk).

---

## 4. Phased roadmap — ordered by dependency and value

Ordering principle: **the operator+undo+keymap-v2 foundation first** (everything
hangs off it), then the visible editor/region chrome, then tools/gizmos, then the
properties/outliner data model, then pies/modes. The already-shipped first slice
(shortcut registry `BINDINGS`+`poll`, the screen-space move gizmo + axis line, the
Add/Patch/Duplicate dialogs, World-as-top-level + `Selection.world`) is the
*pre-foundation* — it gives us the proto-operator registry, the modal-op template
(`TransformOp`), the dialogs that become the F9 redo panel, and the hierarchy
root. The roadmap consumes those, it does not redo them.

**Phase 1 — Foundation: operator + undo + keymap-v2.** *Unblocks everything.*
- 1a (smallest shippable, the headline): `src/ui/op.rs` with `DocSnapshot` /
  `UndoStep{before,after}` / `UndoStack` on `Ui` (64 steps, ~256 MB,
  **excluding** asset blobs). Wire **only four** mutators through capture/push at
  the existing deferred-commit points — Delete (`commit_delete`), Add (Add menu),
  Duplicate (duplicate window), Patch/Unpatch (dialog confirm). Add
  `Action::Undo`/`Redo` + Cmd+Z/Cmd+Shift+Z + Edit menu (step name, greyed when
  empty). No trait yet — inline begin/push. **Result: undo/redo for the discrete
  ops** — the explicitly requested first slice.
- 1b: introduce the `Operator` trait + `OpFlags`/`OpStatus`/`OpCtx` + `Ui::run_op`;
  migrate the four sites onto it; convert G/R/S `TransformOp` confirm/cancel to push
  through the same stack (modal undo); add nudge with grouped coalescing.
- 1c (keymap spine): re-express `BINDINGS` as `Trigger`/`Press`/`Tri`/`Mods`/`Kmi`/
  `KeyMap` (Phase 0 = pure refactor, no behaviour change), then add the
  `KeymapId` context stack + most-specific-first dispatch (retires `s_is_scale`/
  `nudge_ok`), then the Transform Modal Map (`ModalAction` + `poll_modal`
  rewriting viewport's raw key reads).

**Phase 2 — Editor / Header + N/T panels.** *Unblocks tool rail + properties
home; the visible Blender-feel.* Built on `PanelViewer::ui`.
- 2a: Viewport header (`TopBottomPanel::top().show_inside`) hosting Mode+Exposure
  (deletes the floating-overlay hack) + a View menu. Zero new state.
- 2b: Viewport N-panel (`SidePanel::right`, N-key toggle) rendering
  `panels::inspector`. Transform props where the eyes are.
- 2c: Viewport T-panel tool rail (left, T-key toggle) — see Phase 3.
- 2d: generalise to the `Editor` trait so every editor gets a header; add the
  editor-type switcher dropdown (persists for free via `DockState<Tab>`).

**Phase 3 — Tools + Gizmos.** *Builds on Phase 1 op pipeline + Phase 2 T-panel.*
- 3a: `ActiveTool` enum + the tool rail; gate the existing move-gizmo block on
  `active_tool ∈ {Move,Transform}`. (Visible toolbar + working move gizmo = demo.)
- 3b: extract `XformGizmo`/`GizmoGroup` trait (behaviour-identical refactor); add
  rotate rings + scale boxes.
- 3c: Measure tool (read-only, high value, exercises the trait for a non-xform op).
- 3d: Aim tool (the lighting differentiator) — routed through the cue/slew target.
- 3e: tool-settings header (orientation, snap), tool groups, per-mode filtering.

**Phase 4 — Datablock / Outliner / Properties.** *The data-model deepening;
benefits from Phase 1's IDs-in-snapshots.*
- 4a (foundation, ships invisibly): stable `EntityId`/`AssetId` + `id` fields +
  `index_of` + load migration. **Kills the remap-on-delete bug class.**
- 4b: Properties context tab-stack (`PropTab`, `properties_panel`), reusing
  `*_inspector` fns; `Tab::Inspector → Properties` with dock migration. Gives
  RenderSettings its single home.
- 4c: `Visibility{hidden,locked,render}` across all kinds + eye/lock/render
  outliner columns + renderer/pick/capture wiring.
- 4d: `Collection`/`Node` tree + auto-migrate into kind-collections + recursive
  ViewLayer outliner with disclosure + visibility fold-down.
- 4e: user-count derivation + ProjectFile/Orphan outliner modes + Purge unused.

**Phase 5 — Pie / Modes / full keymap.** *The "feels like Blender" capstone;
needs Phase 1 keymap + Phase 2 chrome.*
- 5a: `src/ui/pie.rs` + the Shading pie (Z); then the View pie (`).
- 5b: `enum Mode` + `Mode::editable()` + `Ui.mode`; bind workspaces to modes;
  promote workspaces to top tabs. Mode initially drives status-bar label +
  inspector default category.
- 5c: make Mode **gate** the viewport (pick/gizmo/G-R-S act per `editable()`);
  Mode pie (Tab); the Focus/Look workspace+mode.
- 5d: `Context::Mode(...)` per-mode keymap filtering; the Preferences > Keymap
  editor (preset switch, capture, restore, conflict highlight) + KeyConfig
  diff persistence (sequenced with general preference persistence).
- 5e (long tail): route inspector + DMX-grid field edits through grouped
  operators (debounce a slider-drag into one undo step on release). The bulk of
  call sites — last, after the op stack is proven.

---

## 5. License & scope discipline

**REIMPLEMENT (behaviour/contracts only — clean-room, our own Rust types and
names):** the operator lifecycle + status returns + "system pushes undo after
Finished" rule; the undo-stack push/undo/redo + step/memory caps; F9 adjust-last
(undo+re-exec), F3 search, Repeat-Last; the region layout model (header/main/N/T) +
N/T toggle keys + editor-type-switch; the layered keyconfig + context-stacked
keymaps + tri-state mods + modal keymaps + user-diff persistence + keymap-editor
UX; the active-tool record + gizmo-group callback shape + highlight_part hover +
X=red/Y=green/Z=blue (a de-facto standard) + select-as-fallback; datablock +
user-count + fake-user semantics + library-override behaviour + outliner display
modes + restriction columns + the Properties context tab-stack; the pie radial
gesture + workspace=layout+mode + mode-gates-editability.

**DO NOT copy** any GPL source — `undo_system.c`, `memfile_undo.cc`,
`wm_operators.cc`, `wm_keymap.c`, `wm_toolsystem.c`, `transform_gizmo_3d.cc`,
`DNA_*.h` struct layouts, the outliner buildtree, `interface_templates.c`, the
`blender_default.py` / `industry_compatible_data.py` keymap tables, or the
RNA/macro registration machinery. Generic CS names (Operator, UndoStack, KeyMap)
are fine; struct layouts and code are ours. The big win: **egui_dock (MIT/Apache)
replaces Blender's GPL `ScrVert`/`ScrEdge`/`area_split` graph outright**, so the
hardest GPL subsystem is never reimplemented.

**Deliberately SKIP** (irrelevant to arch-viz or not worth the cost now):
implicit-sharing memfile optimization (our doc is tiny); mode-local fine-grained
undo (sculpt/edit-mesh/text); `OPTYPE_MACRO` chaining; the full RNA property
system + Data-API outliner mode; tool-system per-mesh-mode tool zoo
(Sculpt/Paint/Pose brushes) + `draw_select` offscreen id-picking; NDOF/tablet
events, gizmo VR flags, double-key chords; the SpaceLink stack + space duplication;
RGN_TYPE_NAV_BAR (Inspector uses CollapsingHeaders) + Properties pinning; Object/
ObData mesh-instancing as a literal model; multi-scene + cross-file library
linking + Holdout/Indirect-Only flags + IDProperty custom props + NodeTrees +
animation datablocks (cues are our timeline); Quick-Favourites pie + user
workspace authoring. We keep the menu_bar/status_bar as window chrome, not areas.
**Keep the seam open** for: field-granular `DocSnapshot`, per-property fixture
library overrides, full kmi-properties editing — designed-for, shipped-later.

---

## 6. Open product decisions for the lead

1. **Undo strategy — scene-snapshot vs command-pattern.** The spec recommends
   **full-document bincode snapshot** (simplest correct; proven by project.rs;
   small doc). Confirm we accept ~tens-of-MB worst-case stack cost and that
   snapshots **exclude** asset blobs (GDTF/model/HDRI Arcs). Do we ever need
   command-pattern (memory-cheap, replayable) instead? Recommendation: snapshot
   now, keep `DocSnapshot` field-granular-ready.
2. **Authored vs live-driven split for undo.** Incoming DMX mutates fixtures every
   frame. Confirm undo scope = *authored* state only (positions, patch, cues,
   geometry, optics targets) and that snapshotting a live-driven intensity/colour
   right after a DMX frame restoring a momentarily-stale value (overwritten next
   frame) is acceptable — or do we mark driven fields `#[serde(skip)]` in the
   snapshot path? **Resolve in Phase 1.**
3. **The four modes — names and gating strictness.** Confirm Layout/Patch/Focus/
   Render and their `editable()` bitsets. How strict is gating? (Render = truly
   view-only, or still allow selection?) There is no undo until Phase 1, so a mode
   that wrongly *allows* an edit is unrecoverable — sequence mode-gating after undo.
4. **How far to push area-splitting given egui_dock.** egui_dock gives split/join/
   drag/maximize but **no header hook**; we carve headers from each leaf's `ui`.
   Confirm we accept per-leaf headers drawn by us (vs. waiting for upstream), and
   whether editor-type-switching every leaf is in scope or viewport-only.
5. **Tool rail placement.** Global `SidePanel::left(36px)` (simple, competes with
   the dock's left split) **vs** per-viewport painter overlay (more work, no dock
   fight). Pick before Phase 3a.
6. **Inspector duplication.** Once the viewport N-panel renders `inspector`, do we
   keep `Tab::Inspector`/`Tab::Properties` as a dock tab too? Recommendation: keep
   both (N-panel default-on in Focus/Visualise, Properties tab in Design).
7. **Keymap preset + persistence home.** Confirm shipping `Previz` +
   `IndustryCompatible` presets, and that keymap user-diffs + `Ui.mode` + active
   workspace ride on **general preference/project persistence** (Preferences is
   not yet persisted) — or this pillar ships unsavable.
8. **`.archie` format version + migration.** Adding `id`/`vis`/`root` to the
   positional-bincode `.archie` misaligns all old saves. Confirm we bump a real
   format version with a migration step in project.rs (mandatory; the single
   biggest correctness trap).
