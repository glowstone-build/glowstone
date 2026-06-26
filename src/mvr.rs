//! MVR (My Virtual Rig) scene import/export.
//!
//! An `.mvr` file is a ZIP archive of a `GeneralSceneDescription.xml` scene
//! graph plus the resources it references: embedded `.gdtf` fixture definitions,
//! `.glb`/`.3ds` 3D models for the static stage/rigging geometry, and textures.
//! This module parses the scene graph into a flat list of placed fixtures and
//! static geometry (reusing [`GdtfFixture`] for the embedded fixtures), retains
//! every resource verbatim so a scene can be re-bundled, and writes it back out.
//!
//! ## Coordinate space
//!
//! MVR is right-handed, **+Z-up**, and — the subtle part — a `<Matrix>`
//! translation is in **millimetres** while geometry vertices (in the `.glb`
//! models and the GDTF meshes) are in **metres**. A `<Matrix>` is four
//! 3-component vectors `{Vx}{Vy}{Vz}{O}`: the three basis vectors are the
//! *columns* of the rotation/scale (each is the image of a local axis) and `O`
//! is the origin, so the whole thing maps directly onto a column-major
//! [`Mat4`] once `O` is scaled mm → m.
//!
//! The app world is **+Y-up, metres**. [`mvr_to_world`] (a −90° rotation about
//! X) bridges MVR space to world, exactly mirroring the GDTF import path
//! (`gdtf_to_world` in the renderer). For a fixture/object whose MVR-space
//! placement is `M`, its world placement is `mvr_to_world() * M`.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use glam::{Mat4, Vec3, Vec4};

use crate::gdtf::GdtfFixture;

/// MVR → world basis change: MVR is +Z-up, the app world is +Y-up. A −90°
/// rotation about X sends +Z to +Y (identical to the renderer's GDTF mapping).
pub fn mvr_to_world() -> Mat4 {
    Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)
}

/// glTF/GLB meshes are authored +Y-up (assimp), but the MVR geometry frame the
/// `<Matrix>` operates in is +Z-up. Each model's vertices are flipped into the
/// geometry frame with a +90° X rotation before the placement matrix — the same
/// convention the GDTF part assembly uses.
pub fn glb_yup_to_zup() -> Mat4 {
    Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2)
}

// ---------------------------------------------------------------------------
// Round-trip metadata (carried on the app's Scene so an edited scene can be
// written back out faithfully).
// ---------------------------------------------------------------------------

/// One DMX patch entry: a DMX `break` plus the absolute address. MVR stores the
/// address as a single integer spanning universes (`(addr-1)/512` = universe,
/// `(addr-1)%512` = channel, both effectively 1-based for display).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MvrAddress {
    pub break_id: u32,
    pub absolute: u32,
}

impl MvrAddress {
    /// 1-based universe this address falls in.
    pub fn universe(&self) -> u32 {
        self.absolute.saturating_sub(1) / 512 + 1
    }
    /// 1-based channel within the universe.
    pub fn channel(&self) -> u32 {
        self.absolute.saturating_sub(1) % 512 + 1
    }
}

/// The MVR-specific fields of a placed `<Fixture>` that the app's [`Fixture`]
/// doesn't otherwise model. Kept so an imported scene round-trips on export.
///
/// [`Fixture`]: crate::scene::Fixture
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct MvrFixtureMeta {
    pub uuid: String,
    pub fixture_id: String,
    pub unit_number: i32,
    pub fixture_type_id: i32,
    pub custom_id: i32,
    /// The `GDTFSpec` file name (e.g. "Robe Lighting@Robin Esprite.gdtf").
    pub gdtf_spec: String,
    /// The selected DMX mode name.
    pub gdtf_mode: String,
    pub addresses: Vec<MvrAddress>,
    /// Class UUID (`<Classing>`), if any.
    pub classing: Option<String>,
    /// Position/focus label UUID (`<Position>`), if any.
    pub position: Option<String>,
    pub cast_shadow: bool,
    /// Raw `<CustomCommand>` strings (encoder/blade/zoom defaults), preserved
    /// verbatim for export.
    pub custom_commands: Vec<String>,
    /// The original `<Color>` CIE xyY string, preserved so an unedited fixture
    /// round-trips its colour exactly (rather than through a lossy RGB↔xyY trip).
    pub color_raw: Option<String>,
    /// UUID of the layer this fixture belongs to (for export grouping).
    pub layer: String,
}

/// The MVR-specific fields of a static `<SceneObject>` / `<GroupObject>`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct MvrObjectMeta {
    pub uuid: String,
    pub classing: Option<String>,
    /// The original element tag (`SceneObject`, `Truss`, `Support`,
    /// `VideoScreen`, `Projector`) so export re-emits the same node type.
    pub kind: String,
    pub layer: String,
}

/// A named UUID entry (a Class or a Position in `<AUXData>`, or a `<Layer>`).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MvrNamed {
    pub uuid: String,
    pub name: String,
}

/// Document-level metadata (header + the layer/class/position tables) retained
/// so export can reproduce the `<Scene>` scaffolding. Object membership is
/// driven by the per-object `layer` UUID, not by this table.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MvrHeader {
    pub ver_major: u32,
    pub ver_minor: u32,
    pub provider: String,
    pub provider_version: String,
    pub layers: Vec<MvrNamed>,
    pub classes: Vec<MvrNamed>,
    pub positions: Vec<MvrNamed>,
}

impl Default for MvrHeader {
    fn default() -> Self {
        // Sensible defaults for a scene authored in-app (no source MVR).
        Self {
            ver_major: 1,
            ver_minor: 5,
            provider: "previz".into(),
            provider_version: env!("CARGO_PKG_VERSION").into(),
            layers: Vec::new(),
            classes: Vec::new(),
            positions: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser output
// ---------------------------------------------------------------------------

/// One 3D model reference: the archive file name (also the export reference and
/// the renderer's cache key) plus its raw bytes (a glTF/GLB blob in metres,
/// authored +Y-up).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct GeometryModel {
    pub file: String,
    pub glb: Arc<Vec<u8>>,
    /// The per-`<Geometry3D>` local transform (file frame → the object frame),
    /// or identity if the element carried no `<Matrix>`. This is frequently a
    /// unit-conversion *scale* (e.g. inch-authored models at `0.0254`, or
    /// `0.025` model-units) that MUST be applied — dropping it renders the model
    /// grossly mis-sized (Key Arena's helicopter at ~40× / its PAR cans at ~40×).
    /// Translation is already scaled mm → m (via [`parse_matrix`]); the
    /// rotation/scale part is unitless. Applied as `object.world * matrix * flip`.
    #[serde(default = "mat4_identity")]
    pub matrix: Mat4,
}

/// Serde default for [`GeometryModel::matrix`] (no const fn for `Mat4::IDENTITY`).
fn mat4_identity() -> Mat4 {
    Mat4::IDENTITY
}

/// A parsed, placed fixture: its world-space base transform plus the resolved
/// GDTF definition and the round-trip metadata.
pub struct ImportedFixture {
    pub name: String,
    /// World-space base placement (Y-up, metres), before pan/tilt.
    pub world: Mat4,
    /// The resolved + parsed GDTF (shared across instances of the same type).
    pub gdtf: Option<Arc<GdtfFixture>>,
    /// Fixture tint (linear RGB) from `<Color>`, if present.
    pub color: Option<[f32; 3]>,
    pub meta: MvrFixtureMeta,
}

/// A parsed, placed static object (stage deck, truss, set piece, screen).
pub struct ImportedObject {
    pub name: String,
    /// World-space placement (Y-up, metres) of the object frame. The renderer
    /// applies [`glb_yup_to_zup`] to each model on top of this.
    pub world: Mat4,
    pub models: Vec<GeometryModel>,
    pub meta: MvrObjectMeta,
}

/// The whole parsed MVR scene plus every retained resource (keyed by archive
/// file name) so it can be re-bundled on export.
pub struct MvrImport {
    pub header: MvrHeader,
    pub fixtures: Vec<ImportedFixture>,
    pub objects: Vec<ImportedObject>,
    /// LED video walls parsed from `<VideoScreen>` nodes (placement + the
    /// `previz:` round-trip attributes; falls back to defaults for foreign MVRs).
    pub screens: Vec<crate::scene::LedScreen>,
    /// Every non-XML archive entry, verbatim (gdtf / glb / 3ds / textures).
    pub resources: HashMap<String, Arc<Vec<u8>>>,
}

impl MvrImport {
    pub fn load_path(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Self::load_bytes(&bytes)
    }

    pub fn load_bytes(bytes: &[u8]) -> Result<Self, String> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|e| format!("open mvr zip: {e}"))?;

        // Slurp every entry. The scene description is parsed; everything else is
        // retained verbatim for rendering + re-bundling on export.
        let mut xml: Option<String> = None;
        let mut resources: HashMap<String, Arc<Vec<u8>>> = HashMap::new();
        for i in 0..archive.len() {
            let mut f = match archive.by_index(i) {
                Ok(f) => f,
                Err(e) => {
                    log::warn!("mvr: skipping bad entry {i}: {e}");
                    continue;
                }
            };
            if !f.is_file() {
                continue;
            }
            let name = f.name().to_string();
            // Cap the pre-allocation hint: the declared size is untrusted (a
            // crafted archive could claim gigabytes to force an eager OOM).
            // `read_to_end` still grows to the real decompressed length.
            let mut buf = Vec::with_capacity((f.size() as usize).min(16 * 1024 * 1024));
            if f.read_to_end(&mut buf).is_err() {
                log::warn!("mvr: failed to read entry {name}");
                continue;
            }
            // The scene description can be at the root or (rarely) nested.
            if name.rsplit('/').next() == Some("GeneralSceneDescription.xml") {
                xml = Some(String::from_utf8_lossy(&buf).into_owned());
            } else {
                resources.insert(name, Arc::new(buf));
            }
        }

        let xml = xml.ok_or("GeneralSceneDescription.xml missing")?;
        let doc = roxmltree::Document::parse(&xml).map_err(|e| format!("parse scene xml: {e}"))?;
        let root = doc
            .descendants()
            .find(|n| n.has_tag_name("GeneralSceneDescription"))
            .ok_or("no <GeneralSceneDescription>")?;

        let header = parse_header(&root);

        let scene = root
            .children()
            .find(|n| n.has_tag_name("Scene"))
            .ok_or("no <Scene>")?;

        // Resolve + parse each unique GDTF once, sharing the Arc across instances.
        let mut gdtf_cache: HashMap<String, Option<Arc<GdtfFixture>>> = HashMap::new();

        let mut fixtures = Vec::new();
        let mut objects = Vec::new();
        let mut screens = Vec::new();

        if let Some(layers) = scene.children().find(|n| n.has_tag_name("Layers")) {
            for layer in layers.children().filter(|n| n.has_tag_name("Layer")) {
                let layer_uuid = attr(&layer, "uuid");
                if let Some(children) = layer.children().find(|n| n.has_tag_name("ChildList")) {
                    walk_children(
                        &children,
                        Mat4::IDENTITY,
                        &layer_uuid,
                        &resources,
                        &mut gdtf_cache,
                        &mut fixtures,
                        &mut objects,
                        &mut screens,
                    );
                }
            }
        }

        log::info!(
            "mvr: parsed {} fixtures, {} objects, {} resources (v{}.{}, provider '{}' {})",
            fixtures.len(),
            objects.len(),
            resources.len(),
            header.ver_major,
            header.ver_minor,
            header.provider,
            header.provider_version,
        );

        Ok(MvrImport {
            header,
            fixtures,
            objects,
            screens,
            resources,
        })
    }
}

/// Recursively walk a `<ChildList>`, accumulating the MVR-space transform down
/// through any `<GroupObject>` nesting. Leaves become fixtures or objects with a
/// world-space transform.
#[allow(clippy::too_many_arguments)]
fn walk_children(
    list: &roxmltree::Node,
    parent_mvr: Mat4,
    layer_uuid: &str,
    resources: &HashMap<String, Arc<Vec<u8>>>,
    gdtf_cache: &mut HashMap<String, Option<Arc<GdtfFixture>>>,
    fixtures: &mut Vec<ImportedFixture>,
    objects: &mut Vec<ImportedObject>,
    screens: &mut Vec<crate::scene::LedScreen>,
) {
    for child in list.children().filter(|n| n.is_element()) {
        let local = child
            .children()
            .find(|n| n.has_tag_name("Matrix"))
            .and_then(|m| m.text())
            .and_then(parse_matrix)
            .unwrap_or(Mat4::IDENTITY);
        let mvr_xform = parent_mvr * local;

        match child.tag_name().name() {
            "GroupObject" => {
                if let Some(grandkids) = child.children().find(|n| n.has_tag_name("ChildList")) {
                    walk_children(
                        &grandkids, mvr_xform, layer_uuid, resources, gdtf_cache, fixtures,
                        objects, screens,
                    );
                }
            }
            "Fixture" => {
                fixtures.push(parse_fixture(&child, mvr_xform, layer_uuid, resources, gdtf_cache));
            }
            // An LED video wall → a first-class LedScreen (placement + the
            // previz round-trip attributes; foreign MVRs get sensible defaults).
            "VideoScreen" => {
                screens.push(parse_video_screen(&child, mvr_xform));
            }
            // Static geometry: stage decks, trusses, set pieces, projectors, etc.
            "SceneObject" | "Truss" | "Support" | "Projector" => {
                if let Some(obj) = parse_object(&child, mvr_xform, layer_uuid, resources) {
                    objects.push(obj);
                }
            }
            _ => {}
        }
    }
}

fn parse_fixture(
    node: &roxmltree::Node,
    mvr_xform: Mat4,
    layer_uuid: &str,
    resources: &HashMap<String, Arc<Vec<u8>>>,
    gdtf_cache: &mut HashMap<String, Option<Arc<GdtfFixture>>>,
) -> ImportedFixture {
    let gdtf_spec = child_text(node, "GDTFSpec").unwrap_or_default();
    // Resolve the spec to a canonical archive name first, then cache by *that* —
    // so two fixtures whose specs differ only cosmetically (e.g. "Cluster S2" vs
    // "Roxx@Cluster S2.gdtf") share one parsed definition and one GPU upload.
    let gdtf = if gdtf_spec.is_empty() {
        None
    } else {
        let resolved = matched_name(&gdtf_spec, resources)
            .or_else(|| matched_name(&format!("{gdtf_spec}.gdtf"), resources))
            .unwrap_or_else(|| gdtf_spec.clone());
        gdtf_cache
            .entry(resolved.clone())
            .or_insert_with(|| load_gdtf_named(&resolved, resources))
            .clone()
    };

    let addresses = node
        .children()
        .find(|n| n.has_tag_name("Addresses"))
        .map(|a| {
            a.children()
                .filter(|n| n.has_tag_name("Address"))
                .map(|n| MvrAddress {
                    break_id: n.attribute("break").and_then(|v| v.parse().ok()).unwrap_or(0),
                    absolute: n.text().and_then(|v| v.trim().parse().ok()).unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();

    let custom_commands = node
        .children()
        .find(|n| n.has_tag_name("CustomCommands"))
        .map(|c| {
            c.children()
                .filter(|n| n.has_tag_name("CustomCommand"))
                .filter_map(|n| n.text().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let color_raw = child_text(node, "Color").filter(|s| !s.is_empty());
    let color = color_raw.as_deref().and_then(crate::gdtf::parse_cie_xyy);

    let meta = MvrFixtureMeta {
        uuid: attr(node, "uuid"),
        fixture_id: child_text(node, "FixtureID").unwrap_or_default(),
        unit_number: child_int(node, "UnitNumber"),
        fixture_type_id: child_int(node, "FixtureTypeId"),
        custom_id: child_int(node, "CustomId"),
        gdtf_spec,
        gdtf_mode: child_text(node, "GDTFMode").unwrap_or_default(),
        addresses,
        classing: child_text(node, "Classing").filter(|s| !s.is_empty()),
        position: child_text(node, "Position").filter(|s| !s.is_empty()),
        cast_shadow: child_text(node, "CastShadow")
            .map(|s| s.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false),
        custom_commands,
        color_raw,
        layer: layer_uuid.to_string(),
    };

    ImportedFixture {
        name: attr(node, "name"),
        world: mvr_to_world() * mvr_xform,
        gdtf,
        color,
        meta,
    }
}

fn parse_object(
    node: &roxmltree::Node,
    mvr_xform: Mat4,
    layer_uuid: &str,
    resources: &HashMap<String, Arc<Vec<u8>>>,
) -> Option<ImportedObject> {
    let models: Vec<GeometryModel> = node
        .children()
        .find(|n| n.has_tag_name("Geometries"))
        .into_iter()
        .flat_map(|g| g.children().filter(|n| n.has_tag_name("Geometry3D")))
        .filter_map(|g| {
            let file = g.attribute("fileName").unwrap_or("").to_string();
            if file.is_empty() {
                return None;
            }
            let glb = resolve_resource(&file, resources)?;
            // The `<Geometry3D>` may carry its own `<Matrix>` (a file→object
            // transform, usually a unit-conversion scale). Honour it — ignoring
            // it is what made oversized models trigger the old auto-downscale.
            let matrix = g
                .children()
                .find(|n| n.has_tag_name("Matrix"))
                .and_then(|m| m.text())
                .and_then(parse_matrix)
                .unwrap_or(Mat4::IDENTITY);
            Some(GeometryModel { file, glb, matrix })
        })
        .collect();

    // Skip nodes with no drawable Geometry3D. These are most often Symbol/Symdef
    // instances (block references resolved via AUXData) — not yet supported, so
    // warn rather than silently dropping them from the round-trip.
    if models.is_empty() {
        let has_symbol = node
            .descendants()
            .any(|n| n.has_tag_name("Symbol") || n.has_tag_name("Symdef"));
        if has_symbol {
            log::warn!(
                "mvr: object '{}' uses Symbol/Symdef geometry (not yet supported) — dropped",
                attr(node, "name")
            );
        }
        return None;
    }

    Some(ImportedObject {
        name: attr(node, "name"),
        world: mvr_to_world() * mvr_xform,
        models,
        meta: MvrObjectMeta {
            uuid: attr(node, "uuid"),
            classing: child_text(node, "Classing").filter(|s| !s.is_empty()),
            kind: node.tag_name().name().to_string(),
            layer: layer_uuid.to_string(),
        },
    })
}

/// Parse a `<VideoScreen>` into an [`LedScreen`](crate::scene::LedScreen). MVR has
/// no native cabinet/pitch concept, so the full parametric build is carried in
/// `previz*` round-trip attributes our export writes; a foreign MVR (no such
/// attributes) falls back to a sensible default panel and a `<Sources>`-derived
/// content type.
fn parse_video_screen(node: &roxmltree::Node, mvr_xform: Mat4) -> crate::scene::LedScreen {
    use crate::scene::LedScreen;
    let a_u32 = |k: &str, d: u32| node.attribute(k).and_then(|v| v.parse().ok()).unwrap_or(d);
    let a_f32 = |k: &str, d: f32| node.attribute(k).and_then(|v| v.parse().ok()).unwrap_or(d);

    // Content: prefer our explicit round-trip attributes; else read a `<Source>`
    // node's type (NDI / File / CITP) if present; else a test pattern.
    let content = if let Some(kind) = node.attribute("previzContent") {
        decode_content(kind, node.attribute("previzContentArg").unwrap_or(""))
    } else {
        let src = node
            .descendants()
            .find(|n| n.has_tag_name("Source"));
        match src.and_then(|n| n.attribute("type")) {
            Some("NDI") => crate::scene::screen::ScreenContent::Ndi {
                source: src.and_then(|n| n.text()).unwrap_or("").trim().to_string(),
            },
            Some("CITP") => crate::scene::screen::ScreenContent::Citp {
                source: src.and_then(|n| n.text()).unwrap_or("").trim().to_string(),
            },
            _ => crate::scene::screen::ScreenContent::TestPattern(
                crate::scene::screen::TestPattern::Grid,
            ),
        }
    };

    LedScreen {
        name: attr(node, "name"),
        panel_type: node.attribute("previzPanelType").unwrap_or("Imported").to_string(),
        transform: mvr_to_world() * mvr_xform,
        cabinet_mm: [a_f32("previzCabinetW", 500.0), a_f32("previzCabinetH", 500.0)],
        cabinet_px: [a_u32("previzCabPxX", 128), a_u32("previzCabPxY", 128)],
        panels_wide: a_u32("previzPanelsWide", 4).max(1),
        panels_high: a_u32("previzPanelsHigh", 2).max(1),
        gap_mm: a_f32("previzGap", 0.0),
        curvature_deg: a_f32("previzCurvature", 0.0),
        nits: a_f32("previzNits", 1200.0),
        gamma: a_f32("previzGamma", 2.2),
        opacity: a_f32("previzOpacity", 1.0),
        emit: a_f32("previzEmit", 1.0),
        pixel_shape: match a_u32("previzPixel", 0) {
            1 => crate::scene::screen::PixelShape::SmdSquare,
            2 => crate::scene::screen::PixelShape::DiscreteRgb,
            _ => crate::scene::screen::PixelShape::SmdRound,
        },
        hidden: false,
        id: 0, // assigned by Scene::ensure_ids after import
        content,
        frame: None,
    }
}

/// Encode an LED-screen content source into `(kind, arg)` round-trip attributes.
fn encode_content(c: &crate::scene::screen::ScreenContent) -> (&'static str, String) {
    use crate::scene::screen::ScreenContent as C;
    match c {
        C::TestPattern(p) => ("test", (p.code() as i32).to_string()),
        C::SolidColor(rgb) => ("solid", format!("{},{},{}", rgb[0], rgb[1], rgb[2])),
        C::Image { name, .. } => ("image", name.clone()),
        C::Ndi { source } => ("ndi", source.clone()),
        C::Citp { source } => ("citp", source.clone()),
        C::PixelMapDmx(pm) => {
            ("dmx", format!("{},{},{},{}", pm.cols, pm.rows, pm.universe, pm.start_address))
        }
    }
}

/// Inverse of [`encode_content`]. Image bytes don't round-trip through MVR (only
/// the file name), so a re-imported image shows "no image" until re-picked.
fn decode_content(kind: &str, arg: &str) -> crate::scene::screen::ScreenContent {
    use crate::scene::screen::{PixelMap, ScreenContent as C, TestPattern};
    match kind {
        "solid" => {
            let v: Vec<f32> = arg.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            C::SolidColor([
                *v.first().unwrap_or(&0.5),
                *v.get(1).unwrap_or(&0.5),
                *v.get(2).unwrap_or(&0.5),
            ])
        }
        "image" => C::Image { name: arg.to_string(), bytes: std::sync::Arc::new(Vec::new()) },
        "ndi" => C::Ndi { source: arg.to_string() },
        "citp" => C::Citp { source: arg.to_string() },
        "dmx" => {
            let v: Vec<u32> = arg.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            C::PixelMapDmx(PixelMap {
                cols: (*v.first().unwrap_or(&16)).clamp(1, 256),
                rows: (*v.get(1).unwrap_or(&9)).clamp(1, 256),
                universe: *v.get(2).unwrap_or(&1) as u16,
                start_address: *v.get(3).unwrap_or(&1) as u16,
            })
        }
        _ => {
            let idx: i32 = arg.trim().parse().unwrap_or(0);
            let p = match idx {
                1 => TestPattern::Bars,
                2 => TestPattern::Gradient,
                _ => TestPattern::Grid,
            };
            C::TestPattern(p)
        }
    }
}

fn parse_header(root: &roxmltree::Node) -> MvrHeader {
    let scene = root.children().find(|n| n.has_tag_name("Scene"));
    let aux = scene
        .as_ref()
        .and_then(|s| s.children().find(|n| n.has_tag_name("AUXData")));

    let named = |tag: &str| -> Vec<MvrNamed> {
        aux.as_ref()
            .map(|a| {
                a.children()
                    .filter(|n| n.has_tag_name(tag))
                    .map(|n| MvrNamed { uuid: attr(&n, "uuid"), name: attr(&n, "name") })
                    .collect()
            })
            .unwrap_or_default()
    };

    let layers = scene
        .as_ref()
        .and_then(|s| s.children().find(|n| n.has_tag_name("Layers")))
        .map(|l| {
            l.children()
                .filter(|n| n.has_tag_name("Layer"))
                .map(|n| MvrNamed { uuid: attr(&n, "uuid"), name: attr(&n, "name") })
                .collect()
        })
        .unwrap_or_default();

    MvrHeader {
        ver_major: root.attribute("verMajor").and_then(|v| v.parse().ok()).unwrap_or(1),
        ver_minor: root.attribute("verMinor").and_then(|v| v.parse().ok()).unwrap_or(5),
        provider: root.attribute("provider").unwrap_or("").to_string(),
        provider_version: root.attribute("providerVersion").unwrap_or("").to_string(),
        layers,
        classes: named("Class"),
        positions: named("Position"),
    }
}

// ---------------------------------------------------------------------------
// Resource resolution
// ---------------------------------------------------------------------------

/// Parse an embedded GDTF by its resolved archive name, tagging it with that
/// name + raw bytes so an export can reference and re-bundle the right file.
fn load_gdtf_named(name: &str, resources: &HashMap<String, Arc<Vec<u8>>>) -> Option<Arc<GdtfFixture>> {
    let bytes = resources.get(name)?.clone();
    match GdtfFixture::load_bytes(&bytes) {
        Ok(mut g) => {
            g.spec = name.to_string();
            g.raw = Some(bytes);
            g.source = crate::gdtf::FixtureSource::Mvr;
            Some(Arc::new(g))
        }
        Err(e) => {
            log::warn!("mvr: failed to parse embedded GDTF '{name}': {e}");
            None
        }
    }
}

/// Look up a resource by file name (exact, then basename, then case-insensitive,
/// then suffix/substring) and return its bytes.
fn resolve_resource(want: &str, resources: &HashMap<String, Arc<Vec<u8>>>) -> Option<Arc<Vec<u8>>> {
    matched_name(want, resources).and_then(|n| resources.get(&n).cloned())
}

/// The archive key matching `want`, using the same lenient strategy.
fn matched_name(want: &str, resources: &HashMap<String, Arc<Vec<u8>>>) -> Option<String> {
    if resources.contains_key(want) {
        return Some(want.to_string());
    }
    let want_lc = want.to_lowercase();
    // Exact (case-insensitive) or basename match.
    if let Some(k) = resources.keys().find(|k| {
        k.to_lowercase() == want_lc
            || k.rsplit('/').next().map(str::to_lowercase) == Some(want_lc.clone())
    }) {
        return Some(k.clone());
    }
    // Suffix / substring fallback (handles a dropped manufacturer prefix or
    // extension), preferring the longest match to avoid ambiguity.
    resources
        .keys()
        .filter(|k| {
            let k_lc = k.to_lowercase();
            k_lc.contains(&want_lc) || want_lc.contains(&k_lc)
        })
        .max_by_key(|k| k.len())
        .cloned()
}

// ---------------------------------------------------------------------------
// Small XML + matrix helpers
// ---------------------------------------------------------------------------

fn attr(n: &roxmltree::Node, k: &str) -> String {
    n.attribute(k).unwrap_or("").to_string()
}

/// Text of the first direct child element named `tag`, trimmed.
fn child_text(n: &roxmltree::Node, tag: &str) -> Option<String> {
    n.children()
        .find(|c| c.has_tag_name(tag))
        .and_then(|c| c.text())
        .map(|t| t.trim().to_string())
}

fn child_int(n: &roxmltree::Node, tag: &str) -> i32 {
    child_text(n, tag).and_then(|t| t.parse().ok()).unwrap_or(0)
}

/// Parse an MVR `<Matrix>` — four 3-vectors `{Vx}{Vy}{Vz}{O}` — into a glam
/// [`Mat4`]. The three basis vectors are the rotation/scale *columns*; `O` is the
/// origin in millimetres, converted to metres here. Returns `None` on malformed
/// input (wrong component count).
fn parse_matrix(s: &str) -> Option<Mat4> {
    let nums: Vec<f32> = s
        .split(['{', '}', ','])
        .filter_map(|t| {
            let t = t.trim();
            (!t.is_empty()).then(|| t.parse::<f32>().ok()).flatten()
        })
        .collect();
    if nums.len() != 12 {
        return None;
    }
    const MM_TO_M: f32 = 0.001;
    Some(Mat4::from_cols(
        Vec4::new(nums[0], nums[1], nums[2], 0.0),
        Vec4::new(nums[3], nums[4], nums[5], 0.0),
        Vec4::new(nums[6], nums[7], nums[8], 0.0),
        Vec4::new(
            nums[9] * MM_TO_M,
            nums[10] * MM_TO_M,
            nums[11] * MM_TO_M,
            1.0,
        ),
    ))
}

/// Format a world-space transform back into an MVR `<Matrix>` body string: undo
/// the world→MVR basis change and the metre→millimetre translation scaling.
/// `world` maps the object's local frame into the app world; the returned matrix
/// is in MVR space (Z-up). Used by the exporter.
pub fn format_matrix(world: Mat4) -> String {
    const M_TO_MM: f32 = 1000.0;
    let mvr = mvr_to_world().inverse() * world;
    let c = mvr.to_cols_array_2d(); // c[col][row]
    let v = |col: usize| format!("{{{:.6},{:.6},{:.6}}}", c[col][0], c[col][1], c[col][2]);
    format!(
        "{}{}{}{{{:.6},{:.6},{:.6}}}",
        v(0),
        v(1),
        v(2),
        c[3][0] * M_TO_MM,
        c[3][1] * M_TO_MM,
        c[3][2] * M_TO_MM,
    )
}

/// Format a per-`<Geometry3D>` local matrix back to an MVR `<Matrix>` string.
/// Unlike [`format_matrix`] there is **no** world↔MVR basis change (this matrix
/// lives in the object's own frame); only the metre→millimetre translation
/// scaling is undone — the inverse of how [`parse_matrix`] read it.
fn format_geo_matrix(m: Mat4) -> String {
    const M_TO_MM: f32 = 1000.0;
    let c = m.to_cols_array_2d(); // c[col][row]
    let v = |col: usize| format!("{{{:.6},{:.6},{:.6}}}", c[col][0], c[col][1], c[col][2]);
    format!(
        "{}{}{}{{{:.6},{:.6},{:.6}}}",
        v(0),
        v(1),
        v(2),
        c[3][0] * M_TO_MM,
        c[3][1] * M_TO_MM,
        c[3][2] * M_TO_MM,
    )
}

/// Decompose a rigid world transform into (translation, rotation).
pub fn decompose(world: Mat4) -> (Vec3, glam::Quat) {
    let (_scale, rot, trans) = world.to_scale_rotation_translation();
    (trans, rot)
}

/// Derive the `(position, orientation)` the renderer needs for a fixture whose
/// true world placement is `world` (= [`mvr_to_world`] · M_mvr).
///
/// The renderer reconstructs *every* GDTF fixture as
/// `translate(pos) · from_quat(orient) · gdtf_to_world`, appending its own
/// GDTF→world basis change (correct for app-created fixtures, whose meshes still
/// need +Z-up → +Y-up). For an MVR fixture that basis change is *already* inside
/// `world` via `mvr_to_world`, so we divide the trailing one out here
/// (conjugating the rotation) — otherwise the body and beam pick up a spurious
/// 90° X rotation. Translation is unchanged; [`export_fixture_world`] inverts
/// this for export.
pub fn fixture_base(world: Mat4) -> (Vec3, glam::Quat) {
    decompose(world * mvr_to_world().inverse())
}

/// Reconstruct a fixture's true world placement from the `(position, orientation)`
/// stored by [`fixture_base`] — i.e. re-append the `gdtf_to_world` the renderer
/// adds — so the exporter's [`format_matrix`] recovers the original MVR matrix.
pub fn export_fixture_world(position: Vec3, orientation: glam::Quat) -> Mat4 {
    Mat4::from_translation(position) * Mat4::from_quat(orientation) * mvr_to_world()
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Write the current scene to an `.mvr` file at `path`.
pub fn export_path(scene: &crate::scene::Scene, path: &Path) -> Result<(), String> {
    let bytes = export_bytes(scene)?;
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Serialize the scene to MVR archive bytes: a freshly generated
/// `GeneralSceneDescription.xml` plus every referenced resource (the bundled
/// `.gdtf` fixtures and `.glb` geometry), zipped with deflate.
pub fn export_bytes(scene: &crate::scene::Scene) -> Result<Vec<u8>, String> {
    use std::io::Write;

    let xml = build_xml(scene);

    // Gather resources: start from the original import (textures, models, gdtf),
    // then make sure every fixture's GDTF and every geometry model is present
    // (covers fixtures/objects added or imported separately in-app).
    let mut resources: HashMap<String, Arc<Vec<u8>>> = scene
        .mvr
        .as_ref()
        .map(|m| m.resources.clone())
        .unwrap_or_default();
    for f in &scene.fixtures {
        if let Some(g) = &f.gdtf {
            if !g.spec.is_empty() {
                if let Some(raw) = &g.raw {
                    resources.entry(g.spec.clone()).or_insert_with(|| raw.clone());
                }
            }
        }
    }
    for obj in &scene.geometry {
        for m in &obj.models {
            resources.entry(m.file.clone()).or_insert_with(|| m.glb.clone());
        }
    }

    let mut buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zw.start_file("GeneralSceneDescription.xml", opts)
            .map_err(|e| format!("zip xml: {e}"))?;
        zw.write_all(xml.as_bytes()).map_err(|e| format!("write xml: {e}"))?;
        // Deterministic order for reproducible archives.
        let mut names: Vec<&String> = resources.keys().collect();
        names.sort();
        for name in names {
            zw.start_file(name.as_str(), opts)
                .map_err(|e| format!("zip {name}: {e}"))?;
            zw.write_all(&resources[name])
                .map_err(|e| format!("write {name}: {e}"))?;
        }
        zw.finish().map_err(|e| format!("finish zip: {e}"))?;
    }
    Ok(buf)
}

/// UUID synthesized for an app-created object that has no MVR identity yet. Valid
/// GUID *shape* (uppercase, hyphenated); uniqueness comes from the index.
fn synth_uuid(seed: usize) -> String {
    format!("00000000-0000-4000-8000-{:012X}", seed)
}

/// The default layer for objects with no MVR layer membership (app-created or
/// imported without one).
const DEFAULT_LAYER_UUID: &str = "00000000-0000-4000-8000-505256495A00"; // ...PRVIZ\0

fn build_xml(scene: &crate::scene::Scene) -> String {
    let header = scene.mvr.as_ref().map(|m| &m.header);
    let ver_major = header.map(|h| h.ver_major).unwrap_or(1);
    let ver_minor = header.map(|h| h.ver_minor).unwrap_or(5);
    let provider = "previz";
    let provider_version = env!("CARGO_PKG_VERSION");

    // Group fixtures + objects by layer UUID, preserving the imported layer
    // order, then any extra layers, then a default bucket for orphans.
    let layer_uuid_of = |meta_layer: &str| -> String {
        if meta_layer.is_empty() {
            DEFAULT_LAYER_UUID.to_string()
        } else {
            meta_layer.to_string()
        }
    };

    let mut order: Vec<String> = header.map(|h| h.layers.iter().map(|l| l.uuid.clone()).collect()).unwrap_or_default();
    let mut names: HashMap<String, String> =
        header.map(|h| h.layers.iter().map(|l| (l.uuid.clone(), l.name.clone())).collect()).unwrap_or_default();

    let mut fx_by_layer: HashMap<String, Vec<usize>> = HashMap::new();
    let mut obj_by_layer: HashMap<String, Vec<usize>> = HashMap::new();
    let ensure_layer = |uuid: &str, order: &mut Vec<String>, names: &mut HashMap<String, String>| {
        if !order.iter().any(|u| u == uuid) {
            order.push(uuid.to_string());
            names.entry(uuid.to_string()).or_insert_with(|| {
                if uuid == DEFAULT_LAYER_UUID { "previz".into() } else { "Layer".into() }
            });
        }
    };
    for (i, f) in scene.fixtures.iter().enumerate() {
        let l = layer_uuid_of(f.mvr.as_ref().map(|m| m.layer.as_str()).unwrap_or(""));
        ensure_layer(&l, &mut order, &mut names);
        fx_by_layer.entry(l).or_default().push(i);
    }
    for (i, o) in scene.geometry.iter().enumerate() {
        let l = layer_uuid_of(o.mvr.as_ref().map(|m| m.layer.as_str()).unwrap_or(""));
        ensure_layer(&l, &mut order, &mut names);
        obj_by_layer.entry(l).or_default().push(i);
    }
    // LED screens have no per-screen layer membership — emit them all in the
    // default layer.
    if !scene.screens.is_empty() {
        ensure_layer(DEFAULT_LAYER_UUID, &mut order, &mut names);
    }

    let mut s = String::with_capacity(64 * 1024);
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\" ?>\n");
    s.push_str(&format!(
        "<GeneralSceneDescription verMajor=\"{ver_major}\" verMinor=\"{ver_minor}\" provider=\"{}\" providerVersion=\"{}\">\n",
        xml_escape(provider),
        xml_escape(provider_version),
    ));
    s.push_str("  <Scene>\n    <Layers>\n");

    for layer_uuid in &order {
        let lname = names.get(layer_uuid).cloned().unwrap_or_else(|| "Layer".into());
        let fxs = fx_by_layer.get(layer_uuid).cloned().unwrap_or_default();
        let objs = obj_by_layer.get(layer_uuid).cloned().unwrap_or_default();
        let write_screens = layer_uuid == DEFAULT_LAYER_UUID && !scene.screens.is_empty();
        if fxs.is_empty() && objs.is_empty() && !write_screens {
            s.push_str(&format!(
                "      <Layer name=\"{}\" uuid=\"{}\"/>\n",
                xml_escape(&lname),
                layer_uuid
            ));
            continue;
        }
        s.push_str(&format!(
            "      <Layer name=\"{}\" uuid=\"{}\">\n        <ChildList>\n",
            xml_escape(&lname),
            layer_uuid
        ));
        for &i in &fxs {
            write_fixture(&mut s, &scene.fixtures[i], i);
        }
        for &i in &objs {
            write_object(&mut s, &scene.geometry[i], i);
        }
        if write_screens {
            for (i, sc) in scene.screens.iter().enumerate() {
                write_video_screen(&mut s, sc, i);
            }
        }
        s.push_str("        </ChildList>\n      </Layer>\n");
    }

    s.push_str("    </Layers>\n    <AUXData>\n");
    if let Some(h) = header {
        for c in &h.classes {
            s.push_str(&format!(
                "      <Class name=\"{}\" uuid=\"{}\"/>\n",
                xml_escape(&c.name),
                c.uuid
            ));
        }
        for p in &h.positions {
            s.push_str(&format!(
                "      <Position name=\"{}\" uuid=\"{}\"/>\n",
                xml_escape(&p.name),
                p.uuid
            ));
        }
    }
    s.push_str("    </AUXData>\n  </Scene>\n</GeneralSceneDescription>\n");
    s
}

fn write_fixture(s: &mut String, f: &crate::scene::Fixture, idx: usize) {
    let meta = f.mvr.as_deref();
    let uuid = meta
        .map(|m| m.uuid.clone())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| synth_uuid(idx));
    // MVR placement = base transform without the live (DMX) pan/tilt. Re-append
    // the gdtf_to_world that fixture_base divided out so format_matrix recovers
    // the original MVR matrix.
    let base = export_fixture_world(f.position, f.orientation);
    // Prefer the resolved/bundled GDTF file name so <GDTFSpec> always references
    // a file actually present in the archive (the source spec may be an alias).
    let spec = f
        .gdtf
        .as_ref()
        .map(|g| g.spec.clone())
        .filter(|x| !x.is_empty())
        .or_else(|| meta.map(|m| m.gdtf_spec.clone()).filter(|x| !x.is_empty()))
        .unwrap_or_default();
    let mode = meta
        .map(|m| m.gdtf_mode.clone())
        .filter(|x| !x.is_empty())
        .or_else(|| f.gdtf.as_ref().and_then(|g| g.modes.first().map(|m| m.name.clone())))
        .unwrap_or_default();

    s.push_str(&format!(
        "          <Fixture name=\"{}\" uuid=\"{}\">\n",
        xml_escape(&f.name),
        uuid
    ));
    s.push_str(&format!("            <Matrix>{}</Matrix>\n", format_matrix(base)));

    if let Some(cmds) = meta.map(|m| &m.custom_commands).filter(|c| !c.is_empty()) {
        s.push_str("            <CustomCommands>\n");
        for c in cmds {
            s.push_str(&format!(
                "              <CustomCommand>{}</CustomCommand>\n",
                xml_escape(c)
            ));
        }
        s.push_str("            </CustomCommands>\n");
    }
    if let Some(cls) = meta.and_then(|m| m.classing.as_ref()) {
        s.push_str(&format!("            <Classing>{}</Classing>\n", xml_escape(cls)));
    }
    s.push_str(&format!("            <GDTFSpec>{}</GDTFSpec>\n", xml_escape(&spec)));
    s.push_str(&format!("            <GDTFMode>{}</GDTFMode>\n", xml_escape(&mode)));

    s.push_str("            <Addresses>\n");
    let addrs = meta.map(|m| m.addresses.as_slice()).unwrap_or(&[]);
    if addrs.is_empty() {
        s.push_str("              <Address break=\"0\">1</Address>\n");
    } else {
        for a in addrs {
            s.push_str(&format!(
                "              <Address break=\"{}\">{}</Address>\n",
                a.break_id, a.absolute
            ));
        }
    }
    s.push_str("            </Addresses>\n");

    let fid = meta.map(|m| m.fixture_id.clone()).unwrap_or_default();
    s.push_str(&format!("            <FixtureID>{}</FixtureID>\n", xml_escape(&fid)));
    s.push_str(&format!(
        "            <UnitNumber>{}</UnitNumber>\n",
        meta.map(|m| m.unit_number).unwrap_or(0)
    ));
    s.push_str(&format!(
        "            <FixtureTypeId>{}</FixtureTypeId>\n",
        meta.map(|m| m.fixture_type_id).unwrap_or(0)
    ));
    s.push_str(&format!(
        "            <CustomId>{}</CustomId>\n",
        meta.map(|m| m.custom_id).unwrap_or(0)
    ));
    // Emit the original CIE xyY verbatim when the fixture's colour is unchanged
    // from import (faithful round-trip); otherwise re-encode the edited colour.
    let color = meta
        .and_then(|m| m.color_raw.as_ref())
        .filter(|raw| {
            crate::gdtf::parse_cie_xyy(raw)
                .map(|rgb| {
                    (0..3).all(|i| (rgb[i] - f.color[i]).abs() < 1e-3)
                })
                .unwrap_or(false)
        })
        .cloned()
        .unwrap_or_else(|| {
            let (x, y, big_y) = linear_rgb_to_cie_xyy(f.color);
            format!("{x:.6},{y:.6},{big_y:.6}")
        });
    s.push_str(&format!("            <Color>{color}</Color>\n"));
    s.push_str(&format!(
        "            <CastShadow>{}</CastShadow>\n",
        meta.map(|m| m.cast_shadow).unwrap_or(false)
    ));
    s.push_str("            <Mappings/>\n");
    if let Some(pos) = meta.and_then(|m| m.position.as_ref()) {
        s.push_str(&format!("            <Position>{}</Position>\n", xml_escape(pos)));
    }
    s.push_str("          </Fixture>\n");
}

fn write_object(s: &mut String, o: &crate::scene::SceneGeometry, idx: usize) {
    let meta = o.mvr.as_ref();
    let uuid = meta
        .map(|m| m.uuid.clone())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| synth_uuid(0x1000 + idx));
    // Re-emit the original element type (Truss / Support / VideoScreen / …);
    // default to SceneObject for app-created geometry.
    let kind = meta
        .map(|m| m.kind.as_str())
        .filter(|k| !k.is_empty())
        .unwrap_or("SceneObject");
    s.push_str(&format!(
        "          <{kind} name=\"{}\" uuid=\"{}\">\n",
        xml_escape(&o.name),
        uuid
    ));
    s.push_str(&format!(
        "            <Matrix>{}</Matrix>\n",
        format_matrix(o.transform)
    ));
    s.push_str("            <Geometries>\n");
    for m in &o.models {
        // Re-emit the per-Geometry3D matrix when it isn't identity (so the
        // unit-conversion scale we honoured on import round-trips).
        if m.matrix == Mat4::IDENTITY {
            s.push_str(&format!(
                "              <Geometry3D fileName=\"{}\"/>\n",
                xml_escape(&m.file)
            ));
        } else {
            s.push_str(&format!(
                "              <Geometry3D fileName=\"{}\">\n                <Matrix>{}</Matrix>\n              </Geometry3D>\n",
                xml_escape(&m.file),
                format_geo_matrix(m.matrix),
            ));
        }
    }
    s.push_str("            </Geometries>\n");
    if let Some(cls) = meta.and_then(|m| m.classing.as_ref()) {
        s.push_str(&format!("            <Classing>{}</Classing>\n", xml_escape(cls)));
    }
    s.push_str("            <GDTFSpec></GDTFSpec>\n            <GDTFMode></GDTFMode>\n");
    s.push_str(&format!("          </{kind}>\n"));
}

/// Write an [`LedScreen`](crate::scene::LedScreen) as a `<VideoScreen>` node. The
/// full parametric build (cabinet grid / pitch / nits / content) is carried in
/// `previz*` attributes for a faithful archie round-trip; a `<Sources>` child
/// describes the content type for foreign MVR readers.
fn write_video_screen(s: &mut String, sc: &crate::scene::LedScreen, idx: usize) {
    use crate::scene::screen::ScreenContent as C;
    let uuid = synth_uuid(0x2000 + idx);
    let (kind, arg) = encode_content(&sc.content);
    s.push_str(&format!(
        "          <VideoScreen name=\"{}\" uuid=\"{}\"",
        xml_escape(&sc.name),
        uuid
    ));
    s.push_str(&format!(
        " previzPanelType=\"{}\" previzCabinetW=\"{}\" previzCabinetH=\"{}\" previzCabPxX=\"{}\" previzCabPxY=\"{}\" previzPanelsWide=\"{}\" previzPanelsHigh=\"{}\" previzGap=\"{}\" previzCurvature=\"{}\" previzNits=\"{}\" previzGamma=\"{}\" previzOpacity=\"{}\" previzEmit=\"{}\" previzPixel=\"{}\" previzContent=\"{}\" previzContentArg=\"{}\">\n",
        xml_escape(&sc.panel_type), sc.cabinet_mm[0], sc.cabinet_mm[1], sc.cabinet_px[0], sc.cabinet_px[1],
        sc.panels_wide, sc.panels_high, sc.gap_mm, sc.curvature_deg, sc.nits, sc.gamma, sc.opacity, sc.emit,
        sc.pixel_shape.code() as i32, kind, xml_escape(&arg),
    ));
    s.push_str(&format!("            <Matrix>{}</Matrix>\n", format_matrix(sc.transform)));
    let src_type = match &sc.content {
        C::Ndi { .. } => Some("NDI"),
        C::Citp { .. } => Some("CITP"),
        C::Image { .. } => Some("File"),
        _ => None,
    };
    if let Some(t) = src_type {
        s.push_str(&format!(
            "            <Sources>\n              <Source linkedGeometry=\"\" type=\"{t}\">{}</Source>\n            </Sources>\n",
            xml_escape(&arg)
        ));
    }
    s.push_str("            <Geometries/>\n");
    s.push_str("            <GDTFSpec></GDTFSpec>\n            <GDTFMode></GDTFMode>\n");
    s.push_str("          </VideoScreen>\n");
}

/// Convert linear RGB to a CIE `x,y,Y` string triple (sRGB primaries; `Y` as a
/// 0..100 luminance percentage), the inverse of [`crate::gdtf::parse_cie_xyy`].
fn linear_rgb_to_cie_xyy(rgb: [f32; 3]) -> (f32, f32, f32) {
    let [r, g, b] = rgb;
    let big_x = 0.4124 * r + 0.3576 * g + 0.1805 * b;
    let big_y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let big_z = 0.0193 * r + 0.1192 * g + 0.9505 * b;
    let sum = big_x + big_y + big_z;
    if sum < 1e-6 {
        // Fall back to D65 white chromaticity for a black input.
        return (0.3127, 0.3290, 0.0);
    }
    (big_x / sum, big_y / sum, (big_y * 100.0).clamp(0.0, 100.0))
}

/// Minimal XML text/attribute escaping.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_screen_round_trips_through_mvr() {
        use crate::scene::screen::{LedScreen, ScreenContent};
        use crate::scene::ScreenProfile;
        let prof = ScreenProfile {
            name: "Indoor 3.9mm",
            category: "LED Wall",
            cabinet_mm: [500.0, 500.0],
            cabinet_px: [128, 128],
            gap_mm: 0.0,
            transparent: false,
            default_nits: 1200.0,
        };
        let mut sc = LedScreen::from_profile(
            &prof,
            "Wall A",
            Mat4::from_translation(Vec3::new(1.0, 2.5, -3.0)),
        );
        sc.panels_wide = 6;
        sc.panels_high = 4;
        sc.nits = 4500.0;
        sc.curvature_deg = 20.0;
        sc.content = ScreenContent::Ndi { source: "HOST (Out)".into() };

        let mut scene = crate::scene::Scene::demo();
        scene.fixtures.clear();
        scene.geometry.clear();
        scene.screens.clear();
        scene.screens.push(sc);

        let bytes = export_bytes(&scene).expect("export");
        let imp = MvrImport::load_bytes(&bytes).expect("reimport");
        assert_eq!(imp.screens.len(), 1, "one screen round-trips");
        let s = &imp.screens[0];
        assert_eq!(s.name, "Wall A");
        assert_eq!((s.panels_wide, s.panels_high), (6, 4));
        assert_eq!(s.resolution(), [6 * 128, 4 * 128]);
        assert!((s.nits - 4500.0).abs() < 1e-3);
        assert!((s.curvature_deg - 20.0).abs() < 1e-3);
        assert!(matches!(&s.content, ScreenContent::Ndi { source } if source == "HOST (Out)"));
        let t = s.transform.w_axis.truncate();
        assert!((t - Vec3::new(1.0, 2.5, -3.0)).length() < 1e-2, "translation {t:?}");
    }

    #[test]
    fn matrix_columns_and_units() {
        // 90° about Z (the importer's disambiguating case): {0,1,0}{-1,0,0}{0,0,1}
        // with a 5000 mm / 3000 mm / 0 translation.
        let m = parse_matrix(
            "{-0.000000,1.000000,0.000000}{-1.000000,-0.000000,0.000000}{0.000000,0.000000,1.000000}{5000.0,3000.0,0.0}",
        )
        .expect("parse");
        // Columns are basis-vector images: local +X → world +Y.
        let x_img = m.transform_vector3(Vec3::X);
        assert!((x_img - Vec3::Y).length() < 1e-5, "x_img {x_img:?}");
        // Translation scaled mm → m.
        let o = m.transform_point3(Vec3::ZERO);
        assert!((o - Vec3::new(5.0, 3.0, 0.0)).length() < 1e-5, "o {o:?}");
    }

    #[test]
    fn matrix_round_trips_through_format() {
        let src = "{-1.000000,0.000000,0.000000}{0.000000,-0.500000,0.866025}{0.000000,0.866025,0.500000}{-11.212500,-3131.854677,8780.000000}";
        let m = parse_matrix(src).expect("parse");
        let world = mvr_to_world() * m;
        let back = format_matrix(world);
        let m2 = parse_matrix(&back).expect("reparse");
        // The reconstructed MVR matrix matches the original.
        let a = m.to_cols_array();
        let b = m2.to_cols_array();
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x - y).abs() < 1e-2, "mismatch {x} vs {y}");
        }
    }

    #[test]
    fn malformed_matrix_is_none() {
        assert!(parse_matrix("{1,2,3}{4,5,6}").is_none());
        assert!(parse_matrix("garbage").is_none());
    }

    /// A `<Geometry3D>`'s nested `<Matrix>` (here Key Arena's inch→metre `0.0254`
    /// scale on the helicopter prop) must be parsed onto the model and round-trip
    /// through export — dropping it rendered such models ~40× oversized.
    #[test]
    fn geometry3d_nested_matrix_parsed_applied_and_round_trips() {
        let xml = r#"<R>
          <SceneObject uuid="u" name="heli">
            <Matrix>{1,0,0}{0,1,0}{0,0,1}{0,0,0}</Matrix>
            <Geometries>
              <Geometry3D fileName="scaled.3ds">
                <Matrix>{0.0254,0,0}{0,0.0254,0}{0,0,0.0254}{0,0,0}</Matrix>
              </Geometry3D>
              <Geometry3D fileName="plain.3ds"/>
            </Geometries>
          </SceneObject></R>"#;
        let doc = roxmltree::Document::parse(xml).unwrap();
        let node = doc.descendants().find(|n| n.has_tag_name("SceneObject")).unwrap();
        let mut resources: HashMap<String, Arc<Vec<u8>>> = HashMap::new();
        resources.insert("scaled.3ds".into(), Arc::new(vec![0u8; 4]));
        resources.insert("plain.3ds".into(), Arc::new(vec![0u8; 4]));

        let obj = parse_object(&node, Mat4::IDENTITY, "layer", &resources).expect("object");
        assert_eq!(obj.models.len(), 2);
        // Model with the matrix carries the uniform 0.0254 scale…
        let scaled = obj.models.iter().find(|m| m.file == "scaled.3ds").unwrap();
        let p = scaled.matrix.transform_point3(Vec3::new(1000.0, 0.0, 0.0));
        assert!((p.x - 25.4).abs() < 1e-3, "0.0254 scale applied: {p:?}");
        // …a self-closing `<Geometry3D>` defaults to identity.
        let plain = obj.models.iter().find(|m| m.file == "plain.3ds").unwrap();
        assert_eq!(plain.matrix, Mat4::IDENTITY);

        // Export re-emits the matrix and a re-parse recovers the same scale.
        let re = parse_matrix(&format_geo_matrix(scaled.matrix)).expect("reparse");
        assert!((re.transform_point3(Vec3::X).x - 0.0254).abs() < 1e-6, "round-trip");
    }

    /// Regression guard for the renderer's fixture root composition: the importer
    /// must NOT bake in a basis change the renderer re-applies, or every fixture
    /// body + beam picks up a spurious 90° X rotation.
    #[test]
    fn fixture_base_cancels_renderer_gdtf_to_world() {
        use glam::Quat;
        // A fixture 5 m up with no MVR rotation: identity M_mvr, +5000 mm in Z.
        let m_mvr = parse_matrix("{1,0,0}{0,1,0}{0,0,1}{0,0,5000}").unwrap();
        let world = mvr_to_world() * m_mvr;
        let (pos, orient) = fixture_base(world);

        // Correct world position: 5 m up in +Y.
        assert!((pos - Vec3::new(0.0, 5.0, 0.0)).length() < 1e-4, "pos {pos:?}");
        // No residual rotation — so the renderer's trailing gdtf_to_world is the
        // only basis change, exactly like a plain GDTF fixture at that point.
        assert!(orient.angle_between(Quat::IDENTITY) < 1e-3, "orient {orient:?}");

        // The renderer reconstructs the root as translate·from_quat·gdtf_to_world.
        let root = Mat4::from_translation(pos) * Mat4::from_quat(orient) * mvr_to_world();
        // GDTF beams emit local -Z; under the correct root that is straight DOWN
        // (the bug rendered it as (0,0,+1) — horizontal).
        let dir = root.transform_vector3(Vec3::NEG_Z).normalize();
        assert!((dir - Vec3::NEG_Y).length() < 1e-4, "beam dir {dir:?}, want straight down");

        // And export inverts it back to the original MVR matrix.
        let reparsed = parse_matrix(&format_matrix(export_fixture_world(pos, orient))).unwrap();
        for (a, b) in m_mvr.to_cols_array().iter().zip(reparsed.to_cols_array().iter()) {
            assert!((a - b).abs() < 1e-3, "matrix {a} vs {b}");
        }
    }
}
