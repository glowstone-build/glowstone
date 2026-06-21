# UX / UI: personas, workflow, and the reasoning behind every panel

This is the design rationale for the previz interface. It draws on Blender's
keyboard-first 3D-DCC conventions (the cloned source's `blender_default` /
`industry_compatible` keymaps, workspace tabs, the Properties/N-panel/T-panel
split, the on-viewport shading controls) and on depence R4's lighting-previz model
(Project View + 3D Hierarchy + Settings + **Fixture Manager** + Patching +
Programmer + Show Sequencer + workspace presets; the Patch → Position → Focus →
Colour → Timeline → Visualise workflow). It explains who the tool is for, what
each surface is for, and why it lives where it does.

## Personas

We design for three people. Most users are one of them at a time, and switch hats.

**1. Robin — Lighting Designer / programmer.** At front-of-house or a previz desk,
often in the dark, hours before doors. Goal: *make it look right.* Selects rigs
fast, aims heads, dials colour/intensity/gobo, judges the lit result in 3D.
Keyboard-first; hates hunting menus; lives in the viewport. Values: instant
selection (single, range, by-type), quick look tuning, the beam actually reading
on the set. → drives the **viewport + overlay**, **quick-select (`S`)**,
**Inspector**, **arrow-nudge**, **frame/numpad views**.

**2. Sam — Systems / patch tech.** Builds and verifies the electrical reality:
addresses, universes, modes, conflicts, the incoming console feed. Goal: *every
fixture patched correctly, no clashes.* Thinks in spreadsheets and universes, not
in 3D. Values: a dense table, multi-select + bulk addressing, conflict flags,
live DMX confirmation. → drives the **Fixtures manager**, **DMX grid**,
**Connectivity**, the **Patch workspace**.

**3. Alex — Previz artist / lighting architect.** Produces the picture for the
client, the director, the sales pitch. Goal: *a convincing render of the space.*
Sets the environment (world/HDRI, fog), frames the camera, tunes exposure/bloom,
exports stills. Values: world lighting, a big clean viewport, look controls at
hand. → drives the **World panel**, **viewport overlay**, **Visualise workspace**,
**Preferences > Rendering**.

## The workflow, mapped to the surface

Depence's (and every console's) order is **Patch → Position → Focus → Colour →
Look → Visualise**. Each stage has an obvious home:

| Stage | Who | Where |
|---|---|---|
| Import / Patch | Sam | Library (import GDTF/MVR) → **Fixtures** manager (bulk address) |
| Position / Rig | Robin/Sam | Viewport (click-select, arrow-nudge) + Inspector › Transform |
| Focus / Aim | Robin | Viewport + Inspector › Transform (pan/tilt) |
| Colour / Intensity | Robin | Inspector › Fixture + Optics (dynamic, per-fixture wheels) |
| Look | Robin | Viewport overlay (Mode + Exposure), DMX grid to verify levels |
| Visualise | Alex | World (HDRI), Viewport, Preferences › Rendering |

## Why each panel is where it is

- **Viewport — centre, largest, never closeable.** The product is the picture; it
  gets the real estate and is fixed so the layout can't lose it. *New:* a small
  **display overlay** (Mode segmented + Exposure) sits top-left **on the
  viewport** — Blender puts shading controls on the viewport for the same reason:
  the control belongs where the eyes already are, not in a sidebar you look away
  to reach. The two most-touched look controls (what am I looking at; how bright)
  are one glance away; advanced look (bloom/beam/steps) stays in Preferences.

- **Scene — left, narrow sidebar (~17%).** The outliner: *what is in my show.*
  Three collapsible folders — **Objects** (imported static geometry),
  **Fixtures** (the rig, sortable by Patch/Name/Type, range-selectable), and
  **Environment** + **World** (the visual context). It is for navigating and
  selecting in a spatial/grouped sense, and for setting the world. It is a
  *sidebar* (not centre) because you consult it, you don't stare at it.
  *Removed:* the old **View** render-settings block lived here and duplicated
  Preferences › Rendering — pure redundancy and a second source of truth. Those
  controls now have exactly one home each (overlay / View menu / Preferences), so
  the Scene panel reads as a clean outliner — which is the value the panel should
  carry.

- **Library — left, tabbed with Scene.** *What can I add.* Adjacent to Scene
  because adding-to-scene and seeing-the-scene are the same mental loop. Search +
  sort + multi-select + batch-add.

- **Inspector — right sidebar (~21%).** *The selected thing's properties,* in
  collapsible categories (Transform / Fixture / Optics / Wheels) so a designer
  reaches the field fast and group-edits dynamically (only the controls the
  selection actually exposes). Opposite the outliner (select on the left, edit on
  the right) — the natural left-to-right reading order of "pick → tweak".

- **Fixtures (the manager) — bottom, tabbed with DMX/Connectivity.** *The patch
  schedule.* Renamed from "Patch": it is now a depence-style **Fixture Manager** —
  a dense, sortable, filterable table with **multi-select (synced to the 3D
  selection) and bulk editing** (set universe, patch sequentially, enable/disable).
  This is the panel Sam lives in, and the one the user singled out as "handy
  because you can see a lot of data" — now it earns that by letting you act on the
  data, not just read it. It is at the bottom (a wide strip) because a schedule is
  wide, not tall.

- **DMX — bottom.** The patching grid (depence's "Patching"): the 512-channel
  universe view, the electrical truth, fixture-colour-coded with live levels. Sits
  next to Fixtures because patch-the-fixture and see-the-channels are two views of
  one task. Renamed "DMX Universe" → "DMX" (shorter; the universe selector is
  inside it).

- **Connectivity — bottom.** Art-Net / sACN sources + merge — Sam's network panel,
  grouped with the other data tabs.

## Renames (and why)

- **Patch → Fixtures.** It outgrew "edit one address at a time"; it is the fixture
  sheet now. "Fixtures" says *the rig as data*.
- **DMX Universe → DMX.** Tighter tab; the universe picker lives inside.
- Folder labels in Scene are nouns the audience uses (Objects / Fixtures /
  Environment / World), not implementation terms.

## Shortcuts (cater to the hands)

Aligned with Blender / industry-DCC muscle memory so an artist isn't retrained:

`S` quick-select · `A` select all · `Esc` deselect · click = select, ⌘/Ctrl =
toggle, **Shift = range** · **arrow keys / PageUp·Down nudge** the selected
fixtures on the floor / in height (Shift = 1 m) · `D` duplicate/array · `F` /
`Shift+F` frame selection/all · numpad **7/1/3/5** top/front/right/perspective ·
`L` toggle labels · `⌘/Ctrl+,` preferences. Everything frequent has a key; the
hand stays on the keyboard and the eyes stay in the viewport.

## Workspaces (Window › Workspace)

Three presets re-arrange the dock per stage (Blender's workspace tabs; depence's
Construction/ShowControl/Animation):

- **Design** — balanced everyday layout (outliner + library left, inspector right,
  Fixtures/DMX strip).
- **Patch** — Sam's: a tall Fixtures+DMX data area, thin viewport.
- **Visualise** — Alex's: maximised viewport, thin Scene (World) + Inspector, no
  data strip.

## World / environment (the new lighting model)

A **World** with an equirectangular **HDRI** that both renders as the sky behind
the scene and lights the geometry (image-based ambient), with **brightness**,
ambient strength, and yaw rotation. Depence's "atmosphere/HDR" and the visualiser
persona both demand this: a believable render needs a believable environment, and
the lighting in the room should come from the room, not a flat constant. Lives in
**Scene › World** (it is scene context) with the controls a visualiser reaches for.

## Shipped in the follow-up pass

These were the deferred items; they now exist:

- **Modal transforms (G/R/S).** Grab / rotate / scale the selection in the
  viewport with X/Y/Z axis locks, click·Enter confirm, Esc·right-click (or focus
  loss) cancel, and an on-viewport status line. Rotate also turns each fixture's
  orientation; scale spreads about the selection centroid. (`s` stays the
  quick-select palette when there's nothing selected to scale.)
- **Viewport right-click context menu.** Select same type · Frame · Duplicate ·
  Deselect · Delete (and Select all on empty space).
- **Selection groups.** Scene › Groups: save the current selection as a named
  group, recall by click (highlighted when it matches), delete. Stored
  sorted+deduped; remapped when fixtures are deleted.
- **Cues (a cue list).** A Cues tab: Record the rig's look, Recall/Go with a
  per-cue crossfade (shortest-path pan/tilt, intensity/colour fade), Prev/Go
  transport + fade progress. The offline-previz look engine.

One **deletion path** (`Ui::commit_delete`) remaps the patch, cues and groups in
lock-step with the fixture removal, so deleting a fixture never corrupts
addressing or saved selections/looks.

## Still deferred (and why)

- **3D drag gizmos** (translate/rotate handles grabbed with the mouse). Modal
  G/R/S covers the editing need keyboard-first; interactive gizmo picking is a
  separate viewport-interaction project.
- **A full timeline / Show Sequencer** (cues on a time track with auto-follow,
  multi-part fades, effects). The cue list is the foundation; the timeline editor
  on top is its own piece of work.
