//! GDTF (General Device Type Format) import.
//!
//! A `.gdtf` file is a ZIP archive of a `description.xml` plus glTF model files
//! and wheel/thumbnail images. This module parses the parts we use: fixture
//! identity, wheels (with slot media), 3D models (glTF bytes), the geometry
//! hierarchy (base → pan axis → tilt axis → beam), and the DMX modes/channels.
//!
//! GDTF geometry transforms are column-vector 4×4 matrices (translation in the
//! last column) in a right-handed, +Z-up space; the renderer converts to the
//! app's +Y-up world.

use std::io::Read;
use std::path::Path;

use glam::Mat4;

/// A parsed GDTF fixture definition. The model is intentionally complete; some
/// fields are not yet surfaced in the UI.
#[allow(dead_code)]
#[derive(Clone)]
pub struct GdtfFixture {
    pub name: String,
    pub manufacturer: String,
    pub long_name: String,
    pub short_name: String,
    pub description: String,
    /// Decoded thumbnail PNG bytes, if present.
    pub thumbnail: Option<Vec<u8>>,
    pub wheels: Vec<Wheel>,
    pub models: Vec<Model>,
    /// The primary geometry root (first under `<Geometries>`), with
    /// `GeometryReference`s already expanded. Kept for callers that don't care
    /// about modes; mode-aware callers use [`root_for_mode`](Self::root_for_mode).
    pub geometry: Geometry,
    /// Every top-level geometry under `<Geometries>`, references expanded.
    /// A `<DMXMode Geometry="...">` names which root that mode articulates
    /// (multi-variant fixtures like LED bars ship several rig roots).
    pub roots: Vec<Geometry>,
    pub modes: Vec<DmxMode>,
    /// Beam cone angle in degrees (from the Beam geometry), if present. Kept as
    /// a top-level field for back-compat; the full optics live in [`beam`].
    pub beam_angle: f32,
    /// Physical source/optics parameters from the `Beam` geometry.
    pub beam: BeamData,
    /// The fixture-definition file name — the MVR `GDTFSpec` / on-disk file name
    /// (e.g. "Robe Lighting@Robin Esprite.gdtf"). Empty for ad-hoc loads; set by
    /// [`load_path`](Self::load_path) and the MVR importer so an exported scene
    /// can reference and re-bundle the right `.gdtf`.
    pub spec: String,
    /// The original `.gdtf` archive bytes, retained so an imported scene can be
    /// re-bundled on MVR export without re-reading from disk.
    pub raw: Option<std::sync::Arc<Vec<u8>>>,
}

/// The physical light-source + beam optics declared on the GDTF `Beam`
/// geometry. These drive the source color/intensity and the cone shape.
#[derive(Clone, Debug)]
pub struct BeamData {
    /// Cone angle at 50% intensity, degrees.
    pub beam_angle: f32,
    /// Cone angle at 10% intensity (the soft outer field), degrees.
    pub field_angle: f32,
    /// Physical lens radius where the beam exits, metres.
    pub beam_radius: f32,
    /// "Spot" / "Wash" / "None" / "Rectangle".
    pub beam_type: String,
    /// Correlated color temperature of the source, Kelvin.
    pub color_temp: f32,
    /// Color rendering index (0..100).
    pub cri: f32,
    /// Rated luminous flux, lumens.
    pub luminous_flux: f32,
    /// "LED" / "Halogen" / "Discharge" / "Tungsten".
    pub lamp_type: String,
    /// Throw ratio (distance / image width).
    pub throw_ratio: f32,
    /// Power draw, watts.
    pub power: f32,
}

impl Default for BeamData {
    fn default() -> Self {
        Self {
            beam_angle: 15.0,
            field_angle: 15.0,
            beam_radius: 0.08,
            beam_type: "Spot".into(),
            color_temp: 6500.0,
            cri: 90.0,
            luminous_flux: 10000.0,
            lamp_type: "LED".into(),
            throw_ratio: 1.0,
            power: 300.0,
        }
    }
}

#[derive(Clone)]
pub struct Wheel {
    pub name: String,
    pub slots: Vec<WheelSlot>,
}

#[derive(Clone)]
pub struct WheelSlot {
    pub name: String,
    /// Slot color (linear RGB) derived from the GDTF CIE xyY, if any.
    pub color: Option<[f32; 3]>,
    /// Gobo/animation image PNG bytes, if the slot references media.
    pub media: Option<Vec<u8>>,
    /// Prism facets: each `[dx, dy]` is the beam-deflection offset of one facet
    /// (from the GDTF `Facet` rotation matrix). Empty for non-prism slots.
    pub facets: Vec<[f32; 2]>,
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct Model {
    pub name: String,
    pub file: String,
    pub primitive: String,
    /// width, height, length in metres.
    pub size: [f32; 3],
    /// glTF (GLB) bytes if the model has a file in the archive.
    pub glb: Option<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GeometryKind {
    Geometry,
    /// A rotating axis — drives pan or tilt depending on its attached channel.
    Axis,
    Beam,
    Reference,
    Other,
}

#[derive(Clone)]
pub struct Geometry {
    pub name: String,
    pub kind: GeometryKind,
    pub model: Option<String>,
    /// Transform relative to the parent geometry.
    pub matrix: Mat4,
    pub children: Vec<Geometry>,
    /// Per-node light-source optics, present iff `kind == Beam`. Every `<Beam>`
    /// carries its own angles/radius/flux (a Spiider has four distinct ones).
    pub beam: Option<BeamData>,
    /// `GeometryReference` instancing info, present iff `kind == Reference`
    /// *before* expansion. Expanded instances keep it for DMX break lookups.
    pub reference: Option<GeometryRef>,
}

/// A `<GeometryReference>`: instances a top-level geometry at this node, with
/// the DMX address shifts for channels of the referenced subtree.
#[derive(Clone, Debug)]
pub struct GeometryRef {
    /// Name of the referenced top-level geometry.
    pub target: String,
    /// `<Break>` rows in document order: `(DMXBreak, DMXOffset)`. A channel with
    /// an explicit break uses the first row matching it; a channel with
    /// `DMXBreak="Overwrite"` uses the LAST row.
    pub breaks: Vec<(u32, u32)>,
}

impl GeometryRef {
    /// The 1-based DMX start shift for a channel of this instance.
    /// `dmx_break`: `Some(b)` = explicit break number, `None` = "Overwrite".
    pub fn offset_for(&self, dmx_break: Option<u32>) -> u32 {
        match dmx_break {
            Some(b) => self
                .breaks
                .iter()
                .find(|(brk, _)| *brk == b)
                .map(|&(_, off)| off)
                .unwrap_or(1),
            None => self.breaks.last().map(|&(_, off)| off).unwrap_or(1),
        }
    }
}

/// One light emitter of a fixture in a given mode: a `<Beam>` node instance in
/// the expanded geometry tree. Order matches the assembly walk, so emitter `i`
/// here corresponds to beam frame `i` from `fixture_model::assemble`.
#[derive(Clone, Debug)]
pub struct EmitterDef {
    /// Unique instance name — the enclosing `GeometryReference` name
    /// ("P4 Zone2") or the beam geometry's own name when not referenced.
    pub name: String,
    /// The `<Beam>` optics for this emitter.
    pub beam: BeamData,
    /// When this emitter sits coaxially *behind* another emitter of the same
    /// fixture (it fires through that emitter's aperture — e.g. the Spiider's
    /// "Flower" overlay behind the centre pixel), the front emitter's index.
    /// The renderer draws only the front one; control layers HTP-merge.
    pub merged_into: Option<u16>,
}

#[derive(Clone)]
pub struct DmxMode {
    pub name: String,
    /// Name of the top-level geometry root this mode articulates.
    pub geometry: String,
    pub channels: Vec<DmxChannel>,
    /// The mode's light emitters (expanded from the geometry root).
    pub emitters: Vec<EmitterDef>,
    /// Channels expanded per `GeometryReference` instance with absolute DMX
    /// offsets — what the patch/decode layers actually consume.
    pub resolved: Vec<ResolvedChannel>,
    /// The mode's optical wheel chain — every color/gobo/prism/animation/frost
    /// component the channels expose, in stable (kind, number) order. A fixture
    /// has any number of each; controls align with this list.
    pub components: Vec<OpticalComponent>,
    /// Number of DMX slots the mode occupies (max resolved byte offset).
    pub footprint: u32,
}

/// The kind of an optical-chain wheel component.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum WheelKind {
    Color,
    Gobo,
    Prism,
    Animation,
    Frost,
}

/// Which control of a wheel component a DMX attribute drives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WheelRole {
    /// Slot selection (gobo/color wheels) or insertion amount (prism/frost/animation).
    Value,
    /// Indexed rotation of the inserted element.
    Index,
    /// Continuous rotation/scroll speed (bipolar, 0.5 = stop).
    Spin,
}

/// One wheel component of a mode's optical chain ("Gobo2", "Prism1", …).
#[derive(Clone, Debug)]
pub struct OpticalComponent {
    pub kind: WheelKind,
    /// GDTF attribute number (Gobo3 → 3).
    pub number: u32,
    /// Primary control attribute name ("Gobo3").
    pub attribute: String,
    /// Linked wheel (slot media/colors/facets), from the channel functions.
    pub wheel: Option<String>,
    /// Whether the mode exposes indexed-rotation / continuous-spin controls.
    pub has_index: bool,
    pub has_spin: bool,
}

impl DmxMode {
    /// Index of a component in this mode's chain (controls align with it).
    pub fn component_index(&self, kind: WheelKind, number: u32) -> Option<usize> {
        self.components
            .iter()
            .position(|c| c.kind == kind && c.number == number)
    }
}

/// Classify a GDTF attribute as a wheel-component control: `(kind, number, role)`.
/// `None` for non-wheel attributes (Pan/Dimmer/Zoom/ColorAdd_R/…).
pub fn component_attr(attr: &str) -> Option<(WheelKind, u32, WheelRole)> {
    // Patterns: <Base><N> [suffix]; suffixes map to roles.
    let (kind, rest) = if let Some(r) = attr.strip_prefix("AnimationWheel") {
        (WheelKind::Animation, r)
    } else if let Some(r) = attr.strip_prefix("Gobo") {
        (WheelKind::Gobo, r)
    } else if let Some(r) = attr.strip_prefix("Prism") {
        (WheelKind::Prism, r)
    } else if let Some(r) = attr.strip_prefix("Frost") {
        (WheelKind::Frost, r)
    } else if let Some(r) = attr.strip_prefix("Color") {
        // ColorAdd_*/ColorSub_*/ColorMacro/ColorMixMode etc. are not wheels.
        (WheelKind::Color, r)
    } else {
        return None;
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let number: u32 = digits.parse().ok()?;
    let role = match &rest[digits.len()..] {
        "" => WheelRole::Value,
        "Pos" | "WheelIndex" => WheelRole::Index,
        "PosRotate" | "WheelSpin" | "SelectSpin" => WheelRole::Spin,
        // Shake/audio/macro/random sub-controls aren't modelled (yet); ignore
        // them rather than misroute into the wrong control.
        _ => return None,
    };
    Some((kind, number, role))
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct DmxChannel {
    pub geometry: String,
    pub offsets: Vec<u32>,
    /// DMX break this channel patches into: `Some(n)` or `None` = "Overwrite"
    /// (use the instancing reference's last Break row).
    pub dmx_break: Option<u32>,
    /// Default value normalized to `0..1` (from the InitialFunction's Default).
    pub default: f32,
    /// Primary attribute (the channel's first logical channel).
    pub attribute: String,
    /// First channel-function name (kept for the inspector table).
    pub function: String,
    pub sets: Vec<String>,
    /// Byte resolution: 1 = 8-bit, 2 = 16-bit, … (= `offsets.len()`).
    pub resolution: u8,
    /// Every channel function across this channel's logical channels, in DMX
    /// order. Each carries the physical range and wheel link the engine needs.
    pub functions: Vec<ChannelFunction>,
}

/// One concrete DMX channel of a mode after `GeometryReference` expansion: a
/// channel of a referenced geometry appears once per instance, address-shifted
/// by that instance's Break row.
#[derive(Clone, Debug)]
pub struct ResolvedChannel {
    /// Index into [`DmxMode::channels`] (attribute/functions/resolution live there).
    pub channel: usize,
    /// Absolute 1-based slot offsets within the fixture's footprint, MSB first.
    /// Empty = virtual channel (no DMX footprint; its default still applies).
    pub offsets: Vec<u32>,
    /// The reference instance name when this row came from instancing
    /// ("P4 Zone2"); `None` for direct channels.
    pub instance: Option<String>,
    /// Emitter indices (into [`DmxMode::emitters`]) under this channel's target
    /// geometry — the cells the channel controls. Empty = none (master-only).
    pub cells: Vec<u16>,
    /// Control-group id: rows sharing `(target geometry, instance)` form one
    /// group (= one color layer / master block in the per-cell decode).
    pub group: u16,
}

/// One GDTF `ChannelFunction`: a DMX sub-range mapped linearly onto a physical
/// range, optionally linked to a wheel.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct ChannelFunction {
    pub attribute: String,
    pub name: String,
    /// DMX start value normalized to `0..1` over the channel's full range.
    pub dmx_from: f32,
    pub physical_from: f32,
    pub physical_to: f32,
    /// Linked wheel name (color/gobo/prism/animation), if any.
    pub wheel: Option<String>,
}

impl GdtfFixture {
    pub fn load_path(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let mut f = Self::load_bytes(&bytes)?;
        f.spec = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        Ok(f)
    }

    pub fn load_bytes(bytes: &[u8]) -> Result<Self, String> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|e| format!("open gdtf zip: {e}"))?;

        // Index the archive entry names so we can resolve media/model refs.
        let names: Vec<String> = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
            .collect();

        let xml = read_entry(&mut archive, "description.xml")
            .ok_or("description.xml missing")?;
        let xml = String::from_utf8_lossy(&xml).into_owned();
        let doc = roxmltree::Document::parse(&xml).map_err(|e| format!("parse xml: {e}"))?;

        let ft = doc
            .descendants()
            .find(|n| n.has_tag_name("FixtureType"))
            .ok_or("no FixtureType")?;

        let attr = |n: &roxmltree::Node, k: &str| n.attribute(k).unwrap_or("").to_string();

        let thumb_base = attr(&ft, "Thumbnail");
        let thumbnail = if thumb_base.is_empty() {
            None
        } else {
            resolve(&names, &format!("{thumb_base}.png"))
                .and_then(|n| read_entry(&mut archive, &n))
        };

        // --- wheels ---
        let mut wheels = Vec::new();
        if let Some(ws) = ft.children().find(|n| n.has_tag_name("Wheels")) {
            for w in ws.children().filter(|n| n.has_tag_name("Wheel")) {
                let mut slots = Vec::new();
                for s in w.children().filter(|n| n.has_tag_name("Slot")) {
                    let color = s.attribute("Color").and_then(parse_cie_xyy);
                    let media = s.attribute("MediaFileName").filter(|m| !m.is_empty()).and_then(
                        |m| resolve(&names, &format!("wheels/{m}.png")),
                    );
                    let media = media.and_then(|n| read_entry(&mut archive, &n));
                    let facets = s
                        .children()
                        .filter(|f| f.has_tag_name("Facet"))
                        .filter_map(|f| f.attribute("Rotation").and_then(parse_facet_offset))
                        .collect();
                    slots.push(WheelSlot {
                        name: attr(&s, "Name"),
                        color,
                        media,
                        facets,
                    });
                }
                wheels.push(Wheel {
                    name: attr(&w, "Name"),
                    slots,
                });
            }
        }

        // --- models ---
        let mut models = Vec::new();
        if let Some(ms) = ft.children().find(|n| n.has_tag_name("Models")) {
            for m in ms.children().filter(|n| n.has_tag_name("Model")) {
                let file = attr(&m, "File");
                let glb = if file.is_empty() {
                    None
                } else {
                    // Prefer the high-detail glTF, fall back to the low one.
                    resolve(&names, &format!("models/gltf_high/{file}.glb"))
                        .or_else(|| resolve(&names, &format!("models/gltf/{file}.glb")))
                        .and_then(|n| read_entry(&mut archive, &n))
                };
                let f = |k| m.attribute(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                models.push(Model {
                    name: attr(&m, "Name"),
                    file,
                    primitive: attr(&m, "PrimitiveType"),
                    size: [f("Width"), f("Height"), f("Length")],
                    glb,
                });
            }
        }

        // --- geometry trees: every top-level root, references expanded ---
        let raw_roots: Vec<Geometry> = ft
            .children()
            .find(|n| n.has_tag_name("Geometries"))
            .map(|g| {
                g.children()
                    .filter(|c| c.is_element())
                    .map(|root| parse_geometry(&root))
                    .collect()
            })
            .unwrap_or_default();
        if raw_roots.is_empty() {
            return Err("no geometry".into());
        }
        let roots: Vec<Geometry> = raw_roots
            .iter()
            .map(|r| expand_references(r, &raw_roots, 0))
            .collect();
        let geometry = roots[0].clone();

        // Fixture-level beam summary: the first emitter's optics (back-compat
        // for single-source fixtures; multi-emitter callers use the per-mode
        // emitter list).
        let beam = first_beam(&geometry)
            .or_else(|| roots.iter().find_map(|r| first_beam(r)))
            .cloned()
            .unwrap_or_default();
        let beam_angle = beam.beam_angle;

        // --- DMX modes ---
        let mut modes = Vec::new();
        if let Some(dm) = ft.children().find(|n| n.has_tag_name("DMXModes")) {
            for mode in dm.children().filter(|n| n.has_tag_name("DMXMode")) {
                let mut channels = Vec::new();
                if let Some(chs) = mode.children().find(|n| n.has_tag_name("DMXChannels")) {
                    for ch in chs.children().filter(|n| n.has_tag_name("DMXChannel")) {
                        let offsets: Vec<u32> = ch
                            .attribute("Offset")
                            .unwrap_or("")
                            .split(',')
                            .filter_map(|s| s.trim().parse::<u32>().ok())
                            .collect();
                        let resolution = offsets.len().max(1) as u8;
                        // "Overwrite" (or absent on a referenced channel) → None;
                        // otherwise the explicit break number (default 1).
                        let dmx_break = match ch.attribute("DMXBreak") {
                            Some(b) if b.eq_ignore_ascii_case("overwrite") => None,
                            Some(b) => b.trim().parse::<u32>().ok().or(Some(1)),
                            None => Some(1),
                        };
                        let initial = ch.attribute("InitialFunction").unwrap_or("");

                        // Every channel function across every logical channel.
                        let mut functions = Vec::new();
                        let mut default = f32::NAN; // first CF default until InitialFunction matches
                        for lc in ch.children().filter(|n| n.has_tag_name("LogicalChannel")) {
                            let lc_attr = lc.attribute("Attribute").unwrap_or("");
                            for cf in lc.children().filter(|c| c.has_tag_name("ChannelFunction")) {
                                let pf = cf.attribute("PhysicalFrom").and_then(|v| v.parse().ok());
                                let pt = cf.attribute("PhysicalTo").and_then(|v| v.parse().ok());
                                if let Some(d) = cf.attribute("Default") {
                                    // InitialFunction is "Chan.Logical.Function"; match the leaf.
                                    let name = cf.attribute("Name").unwrap_or("");
                                    let is_initial = !initial.is_empty()
                                        && initial.rsplit('.').next() == Some(name);
                                    if is_initial || default.is_nan() {
                                        default = parse_dmx_norm(d);
                                    }
                                }
                                functions.push(ChannelFunction {
                                    attribute: cf.attribute("Attribute").unwrap_or(lc_attr).to_string(),
                                    name: cf.attribute("Name").unwrap_or("").to_string(),
                                    dmx_from: cf.attribute("DMXFrom").map(parse_dmx_norm).unwrap_or(0.0),
                                    physical_from: pf.unwrap_or(0.0),
                                    physical_to: pt.unwrap_or(1.0),
                                    wheel: cf.attribute("Wheel").filter(|w| !w.is_empty()).map(String::from),
                                });
                            }
                        }

                        let lc = ch.children().find(|n| n.has_tag_name("LogicalChannel"));
                        let attribute = lc
                            .and_then(|n| n.attribute("Attribute"))
                            .unwrap_or("")
                            .to_string();
                        let cf = lc.and_then(|n| n.children().find(|c| c.has_tag_name("ChannelFunction")));
                        let function = cf
                            .and_then(|n| n.attribute("Name"))
                            .unwrap_or("")
                            .to_string();
                        let sets = cf
                            .map(|n| {
                                n.children()
                                    .filter(|c| c.has_tag_name("ChannelSet"))
                                    .map(|c| c.attribute("Name").unwrap_or("").to_string())
                                    .collect()
                            })
                            .unwrap_or_default();
                        channels.push(DmxChannel {
                            geometry: attr(&ch, "Geometry"),
                            offsets,
                            dmx_break,
                            default: if default.is_nan() { 0.0 } else { default },
                            attribute,
                            function,
                            sets,
                            resolution,
                            functions,
                        });
                    }
                }
                let mode_geometry = attr(&mode, "Geometry");
                let root = roots
                    .iter()
                    .find(|r| r.name == mode_geometry)
                    .unwrap_or(&roots[0]);
                let emitters = collect_emitters(root);
                let resolved = resolve_channels(&channels, root, &emitters);
                let components = collect_components(&channels);
                let footprint = resolved
                    .iter()
                    .flat_map(|rc| rc.offsets.iter().copied())
                    .max()
                    .unwrap_or(0);
                modes.push(DmxMode {
                    name: attr(&mode, "Name"),
                    geometry: mode_geometry,
                    channels,
                    emitters,
                    resolved,
                    components,
                    footprint,
                });
            }
        }

        Ok(GdtfFixture {
            name: attr(&ft, "Name"),
            manufacturer: attr(&ft, "Manufacturer"),
            long_name: attr(&ft, "LongName"),
            short_name: attr(&ft, "ShortName"),
            description: attr(&ft, "Description"),
            thumbnail,
            wheels,
            models,
            geometry,
            roots,
            modes,
            beam_angle,
            beam,
            spec: String::new(),
            raw: Some(std::sync::Arc::new(bytes.to_vec())),
        })
    }

    /// The expanded geometry root a DMX mode articulates (falls back to the
    /// primary root for out-of-range modes or unknown root names).
    pub fn root_for_mode(&self, mode_index: usize) -> &Geometry {
        self.modes
            .get(mode_index)
            .and_then(|m| self.roots.iter().find(|r| r.name == m.geometry))
            .unwrap_or(&self.geometry)
    }

    /// The light emitters of a mode (empty slice for out-of-range modes — the
    /// renderer then falls back to the single legacy beam).
    pub fn emitters(&self, mode_index: usize) -> &[EmitterDef] {
        self.modes
            .get(mode_index)
            .map(|m| m.emitters.as_slice())
            .unwrap_or(&[])
    }

    /// Look up a wheel by name.
    pub fn wheel(&self, name: &str) -> Option<&Wheel> {
        self.wheels.iter().find(|w| w.name == name)
    }

    /// The first channel function for `attribute` in the first DMX mode, if the
    /// fixture exposes that attribute. Carries the physical range + wheel link.
    pub fn channel_function(&self, attribute: &str) -> Option<&ChannelFunction> {
        self.modes.first()?.channels.iter().find_map(|c| {
            c.functions.iter().find(|f| f.attribute == attribute)
        })
    }

    /// The physical `(from, to)` range mapped by `attribute`'s first channel
    /// function (e.g. Zoom → (7.8, 58)). `None` if the fixture lacks it.
    pub fn physical_range(&self, attribute: &str) -> Option<(f32, f32)> {
        let f = self.channel_function(attribute)?;
        Some((f.physical_from, f.physical_to))
    }

    /// Whether the fixture exposes a control attribute at all.
    pub fn has_attribute(&self, attribute: &str) -> bool {
        self.modes
            .first()
            .map(|m| m.channels.iter().any(|c| c.functions.iter().any(|f| f.attribute == attribute)))
            .unwrap_or(false)
    }

    /// Geometry name driven by an attribute (e.g. "Pan", "Tilt") in mode 0.
    pub fn geometry_for_attribute(&self, attribute: &str) -> Option<&str> {
        self.modes.first()?.channels.iter().find_map(|c| {
            if c.attribute == attribute {
                Some(c.geometry.as_str())
            } else {
                None
            }
        })
    }
}

fn parse_geometry(node: &roxmltree::Node) -> Geometry {
    let kind = match node.tag_name().name() {
        "Geometry" => GeometryKind::Geometry,
        "Axis" => GeometryKind::Axis,
        "Beam" => GeometryKind::Beam,
        "GeometryReference" => GeometryKind::Reference,
        _ => GeometryKind::Other,
    };
    let matrix = node
        .attribute("Position")
        .and_then(parse_matrix)
        .unwrap_or(Mat4::IDENTITY);
    let children = node
        .children()
        .filter(|c| {
            matches!(
                c.tag_name().name(),
                "Geometry" | "Axis" | "Beam" | "GeometryReference" | "FilterColor" | "FilterGobo"
            )
        })
        .map(|c| parse_geometry(&c))
        .collect();
    let beam = (kind == GeometryKind::Beam).then(|| {
        let f = |k: &str, d: f32| node.attribute(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
        let s = |k: &str, d: &str| node.attribute(k).unwrap_or(d).to_string();
        let beam_angle = f("BeamAngle", 25.0);
        BeamData {
            beam_angle,
            field_angle: f("FieldAngle", beam_angle),
            // Spec defaults: BeamRadius 0.05 m, BeamType Wash (real spots declare
            // "Spot" explicitly — Khamsin does; LED arrays rely on the default).
            beam_radius: f("BeamRadius", 0.05),
            beam_type: s("BeamType", "Wash"),
            color_temp: f("ColorTemperature", 6500.0),
            cri: f("ColorRenderingIndex", 90.0),
            // 0 = unspecified: the renderer splits a nominal fixture flux across
            // the emitter count (many pixel-bar GDTFs omit per-cell flux; a
            // 10 klm default PER CELL would make a 60-cell blinder outshine the
            // whole rig).
            luminous_flux: f("LuminousFlux", 0.0),
            lamp_type: s("LampType", "LED"),
            throw_ratio: f("ThrowRatio", 1.0),
            power: f("PowerConsumption", 300.0),
        }
    });
    let reference = (kind == GeometryKind::Reference).then(|| GeometryRef {
        target: node.attribute("Geometry").unwrap_or("").to_string(),
        breaks: node
            .children()
            .filter(|c| c.has_tag_name("Break"))
            .map(|b| {
                let n = |k: &str, d: u32| {
                    b.attribute(k).and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(d)
                };
                (n("DMXBreak", 1), n("DMXOffset", 1))
            })
            .collect(),
    });
    Geometry {
        name: node.attribute("Name").unwrap_or("").to_string(),
        kind,
        model: node.attribute("Model").map(|s| s.to_string()),
        matrix,
        children,
        beam,
        reference,
    }
}

/// Expand `GeometryReference` nodes into copies of their referenced top-level
/// geometry. The instance keeps the reference's name, transform, break rows and
/// (if set) model; the target's own children/kind/beam fold in underneath. The
/// referenced subtree may itself contain references (depth-capped against
/// cycles, which the spec forbids anyway).
fn expand_references(node: &Geometry, roots: &[Geometry], indirections: u32) -> Geometry {
    let mut out = node.clone();
    let mut next = indirections;
    if node.kind == GeometryKind::Reference && indirections < 4 {
        if let Some(target) = node
            .reference
            .as_ref()
            .and_then(|r| roots.iter().find(|g| g.name == r.target))
        {
            // The reference's Position places the instance; the target root's
            // own matrix is identity by authoring convention (verified across
            // Robe/Astera/Roxx files) and is intentionally ignored.
            out.kind = target.kind;
            out.beam = target.beam.clone();
            out.model = node.model.clone().or_else(|| target.model.clone());
            out.children = target.children.clone();
            // Only reference→reference chains count toward the cycle cap —
            // plain tree depth must not exhaust it (rigs nest 6+ levels).
            next += 1;
        }
    }
    out.children = out
        .children
        .iter()
        .map(|c| expand_references(c, roots, next))
        .collect();
    out
}

/// Derive a mode's optical wheel chain from its channels: every distinct
/// `(kind, number)` seen across channel/function attributes becomes one
/// component, with the wheel link and index/spin capabilities folded in.
fn collect_components(channels: &[DmxChannel]) -> Vec<OpticalComponent> {
    let mut out: Vec<OpticalComponent> = Vec::new();
    let mut note = |attr: &str, wheel: &Option<String>| {
        let Some((kind, number, role)) = component_attr(attr) else {
            return;
        };
        let entry = match out.iter_mut().find(|c| c.kind == kind && c.number == number) {
            Some(e) => e,
            None => {
                out.push(OpticalComponent {
                    kind,
                    number,
                    attribute: format!(
                        "{}{}",
                        match kind {
                            WheelKind::Color => "Color",
                            WheelKind::Gobo => "Gobo",
                            WheelKind::Prism => "Prism",
                            WheelKind::Animation => "AnimationWheel",
                            WheelKind::Frost => "Frost",
                        },
                        number
                    ),
                    wheel: None,
                    has_index: false,
                    has_spin: false,
                });
                out.last_mut().unwrap()
            }
        };
        match role {
            WheelRole::Index => entry.has_index = true,
            WheelRole::Spin => entry.has_spin = true,
            WheelRole::Value => {}
        }
        if entry.wheel.is_none() && wheel.is_some() {
            entry.wheel = wheel.clone();
        }
    };
    for ch in channels {
        note(&ch.attribute, &None);
        for f in &ch.functions {
            note(&f.attribute, &f.wheel);
        }
    }
    out.sort_by(|a, b| (a.kind, a.number).cmp(&(b.kind, b.number)));
    out
}

/// First `<Beam>` optics in a tree, depth-first (the fixture-level summary).
fn first_beam(node: &Geometry) -> Option<&BeamData> {
    if let Some(b) = &node.beam {
        return Some(b);
    }
    node.children.iter().find_map(first_beam)
}

/// Collect every emitter (`<Beam>` instance) of an expanded tree, in the same
/// depth-first order the per-frame assembly walk visits them, and mark
/// coaxially-occluded emitters (rest pose — relative placement within the head
/// is rigid, so the merge decision is pose-independent).
fn collect_emitters(root: &Geometry) -> Vec<EmitterDef> {
    fn rec(node: &Geometry, world: Mat4, out: &mut Vec<(EmitterDef, glam::Vec3, glam::Vec3)>) {
        let world = world * node.matrix;
        if let Some(beam) = &node.beam {
            let origin = world.transform_point3(glam::Vec3::ZERO);
            let dir = world
                .transform_vector3(glam::Vec3::NEG_Z)
                .normalize_or_zero();
            out.push((
                EmitterDef { name: node.name.clone(), beam: beam.clone(), merged_into: None },
                origin,
                dir,
            ));
        }
        for c in &node.children {
            rec(c, world, out);
        }
    }
    let mut tagged = Vec::new();
    rec(root, Mat4::IDENTITY, &mut tagged);

    // Emitter A merges into B when both point the same way, A's axis passes
    // through B's aperture, and A sits behind B — A's light exits through B's
    // lens, so they are one controllable aperture (HTP at the control layer).
    for a in 0..tagged.len() {
        let (origin_a, dir_a) = (tagged[a].1, tagged[a].2);
        let mut best: Option<(u16, f32)> = None;
        for b in 0..tagged.len() {
            if a == b || tagged[b].0.merged_into.is_some() {
                continue;
            }
            let (origin_b, dir_b) = (tagged[b].1, tagged[b].2);
            if dir_a.dot(dir_b) < 0.999 {
                continue;
            }
            let rel = origin_a - origin_b;
            let behind = rel.dot(dir_b);
            if behind >= -1e-4 {
                continue; // A is in front of (or beside) B
            }
            let lateral = (rel - dir_b * behind).length();
            if lateral < tagged[b].0.beam.beam_radius * 0.9
                && best.map(|(_, d)| -behind < d).unwrap_or(true)
            {
                best = Some((b as u16, -behind));
            }
        }
        tagged[a].0.merged_into = best.map(|(b, _)| b);
    }
    tagged.into_iter().map(|(e, _, _)| e).collect()
}

/// Expand a mode's channels per `GeometryReference` instance and resolve each
/// row's absolute DMX offsets and covered emitter cells.
///
/// A channel targets a geometry by name. In the expanded tree that name may
/// appear (a) once, directly — one row, offsets as authored; or (b) as/inside a
/// referenced instance — one row per instance, offsets shifted by the
/// instance's Break (`offset + DMXOffset − 1`). The covered cells are the
/// emitters inside the matched subtree, used by the per-cell decode.
fn resolve_channels(
    channels: &[DmxChannel],
    root: &Geometry,
    emitters: &[EmitterDef],
) -> Vec<ResolvedChannel> {
    struct Hit {
        channel: usize,
        instance: Option<(String, GeometryRef)>,
        cells: Vec<u16>,
    }
    // Depth-first walk mirroring `collect_emitters` order. At each node, match
    // every channel against the node's name — and, for an expanded reference
    // instance (which kept the reference's name), against the referenced
    // TARGET name, since channels are authored against the target ("Lens2").
    // The node's subtree emitter range becomes the hit's covered cells.
    fn walk(
        node: &Geometry,
        channels: &[DmxChannel],
        instance: Option<&(String, GeometryRef)>,
        counter: &mut u16,
        hits: &mut Vec<Hit>,
    ) {
        let this_instance = node
            .reference
            .as_ref()
            .map(|r| (node.name.clone(), r.clone()));
        let scope = this_instance.as_ref().or(instance);

        let start = *counter;
        if node.beam.is_some() {
            *counter += 1;
        }
        let mut pending: Vec<usize> = Vec::new();
        for (i, ch) in channels.iter().enumerate() {
            let matches = ch.geometry == node.name
                || node
                    .reference
                    .as_ref()
                    .is_some_and(|r| r.target == ch.geometry);
            if matches {
                pending.push(hits.len());
                hits.push(Hit { channel: i, instance: scope.cloned(), cells: Vec::new() });
            }
        }
        for c in &node.children {
            walk(c, channels, scope, counter, hits);
        }
        for h in pending {
            hits[h].cells = (start..*counter).collect();
        }
    }
    let mut hits: Vec<Hit> = Vec::new();
    let mut counter = 0u16;
    walk(root, channels, None, &mut counter, &mut hits);
    debug_assert_eq!(counter as usize, emitters.len());

    // A channel whose geometry name matches nothing in this root (sloppy
    // authoring / cross-root names) still needs a row, or it would vanish from
    // the footprint and decode — keep it as a direct master channel.
    for (i, _) in channels.iter().enumerate() {
        if !hits.iter().any(|h| h.channel == i) {
            hits.push(Hit { channel: i, instance: None, cells: Vec::new() });
        }
    }

    // Group rows by (target geometry, instance) — the decode's layer unit.
    let mut groups: Vec<(String, Option<String>)> = Vec::new();
    hits.into_iter()
        .map(|h| {
            let ch = &channels[h.channel];
            let shift = h
                .instance
                .as_ref()
                .map(|(_, r)| r.offset_for(ch.dmx_break))
                .unwrap_or(1);
            let instance = h.instance.map(|(name, _)| name);
            let key = (ch.geometry.clone(), instance.clone());
            let group = match groups.iter().position(|g| *g == key) {
                Some(g) => g as u16,
                None => {
                    groups.push(key);
                    (groups.len() - 1) as u16
                }
            };
            ResolvedChannel {
                channel: h.channel,
                offsets: ch.offsets.iter().map(|o| o + shift - 1).collect(),
                instance,
                cells: h.cells,
                group,
            }
        })
        .collect()
}

/// Parse a GDTF geometry matrix `{Ux,Uy,Uz,Ox}{Vx,Vy,Vz,Oy}{Wx,Wy,Wz,Oz}{0,0,0,1}`.
///
/// Same convention as the MVR `<Matrix>` (see `src/mvr.rs`): the first three
/// lines are the U/V/W **basis vectors** — the images of the local X/Y/Z axes,
/// i.e. the *columns* of the rotation — with the translation in each line's
/// fourth element. Reading the lines as plain row-major matrix rows transposes
/// the rotation (latent for the axis-aligned matrices in most files, visibly
/// wrong for rotated multi-emitter arrays).
fn parse_matrix(s: &str) -> Option<Mat4> {
    let nums: Vec<f32> = s
        .split(['{', '}', ','])
        .filter_map(|t| {
            let t = t.trim();
            if t.is_empty() { None } else { t.parse::<f32>().ok() }
        })
        .collect();
    if nums.len() != 16 {
        return None;
    }
    Some(Mat4::from_cols_array(&[
        nums[0], nums[1], nums[2], 0.0, // col 0 = U (line 1)
        nums[4], nums[5], nums[6], 0.0, // col 1 = V (line 2)
        nums[8], nums[9], nums[10], 0.0, // col 2 = W (line 3)
        nums[3], nums[7], nums[11], 1.0, // translation = line fourth elements
    ]))
}

/// Parse a GDTF prism `Facet` rotation matrix `{a,b,c}{d,e,f}{dx,dy,1}` and
/// return the facet's beam-deflection offset `[dx, dy]` (its 3rd row's x,y).
fn parse_facet_offset(s: &str) -> Option<[f32; 2]> {
    let nums: Vec<f32> = s
        .split(['{', '}', ','])
        .filter_map(|t| {
            let t = t.trim();
            if t.is_empty() { None } else { t.parse::<f32>().ok() }
        })
        .collect();
    if nums.len() < 8 {
        return None;
    }
    Some([nums[6], nums[7]])
}

/// Parse a GDTF DMX value `"value/bytes"` into a `0..1` fraction over the
/// channel's full range (e.g. `"32768/2"` → 0.5, `"11/1"` → 0.043).
fn parse_dmx_norm(s: &str) -> f32 {
    let mut it = s.split('/');
    let value: f64 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0.0);
    let bytes: u32 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(1);
    let max = 2f64.powi(8 * bytes as i32);
    (value / max).clamp(0.0, 1.0) as f32
}

/// Parse a GDTF/MVR CIE `x,y,Y` color string into approximate linear RGB. Shared
/// with the MVR importer (a `<Fixture>`'s `<Color>` uses the same xyY format).
pub(crate) fn parse_cie_xyy(s: &str) -> Option<[f32; 3]> {
    let v: Vec<f32> = s.split(',').filter_map(|t| t.trim().parse().ok()).collect();
    if v.len() < 2 {
        return None;
    }
    let (x, y) = (v[0], v[1]);
    if y <= 1e-5 {
        return Some([1.0, 1.0, 1.0]);
    }
    // xyY (Y normalized to 1) -> XYZ -> linear sRGB.
    let big_y = 1.0;
    let big_x = (x / y) * big_y;
    let big_z = ((1.0 - x - y) / y) * big_y;
    let r = 3.2406 * big_x - 1.5372 * big_y - 0.4986 * big_z;
    let g = -0.9689 * big_x + 1.8758 * big_y + 0.0415 * big_z;
    let b = 0.0557 * big_x - 0.2040 * big_y + 1.0570 * big_z;
    let norm = r.max(g).max(b).max(1.0);
    Some([
        (r / norm).clamp(0.0, 1.0),
        (g / norm).clamp(0.0, 1.0),
        (b / norm).clamp(0.0, 1.0),
    ])
}

/// Resolve a referenced file name against the archive's entries
/// (case-insensitive; some packers vary case or use sub-paths).
fn resolve(names: &[String], want: &str) -> Option<String> {
    if let Some(n) = names.iter().find(|n| n.as_str() == want) {
        return Some(n.clone());
    }
    let want_lc = want.to_lowercase();
    names
        .iter()
        .find(|n| n.to_lowercase() == want_lc)
        .cloned()
}

fn read_entry(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<Vec<u8>> {
    let mut f = archive.by_name(name).ok()?;
    // Cap the pre-allocation hint: the declared size is untrusted (a crafted
    // archive could claim gigabytes). `read_to_end` still grows to real length.
    let mut buf = Vec::with_capacity((f.size() as usize).min(16 * 1024 * 1024));
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end check of the optical extraction against the bundled Khamsin.
    /// Skips silently if the test fixture isn't present on this machine.
    #[test]
    fn khamsin_optics() {
        let path = format!(
            "{}/Downloads/Ayrton@Khamsin@V2.22_New_SVG.gdtf",
            std::env::var("HOME").unwrap_or_default()
        );
        if !std::path::Path::new(&path).exists() {
            eprintln!("skip: {path} not found");
            return;
        }
        let g = GdtfFixture::load_path(std::path::Path::new(&path)).expect("load");

        // Source physics.
        assert_eq!(g.beam.lamp_type, "LED");
        assert!((g.beam.color_temp - 6800.0).abs() < 1.0);
        assert!((g.beam.luminous_flux - 40000.0).abs() < 1.0);
        assert!((g.beam.beam_angle - 25.0).abs() < 0.1);

        // Optical attribute ranges.
        let zoom = g.physical_range("Zoom").expect("zoom");
        assert!((zoom.0 - 7.8).abs() < 0.1 && (zoom.1 - 58.0).abs() < 0.1, "zoom {zoom:?}");
        let iris = g.physical_range("Iris").expect("iris");
        assert!((iris.0 - 1.0).abs() < 0.01 && (iris.1 - 0.15).abs() < 0.01, "iris {iris:?}");
        assert!(g.has_attribute("Frost1"));
        assert!(g.has_attribute("ColorSub_C"));
        assert!(g.has_attribute("Prism1"));
        assert!(g.has_attribute("AnimationWheel1"));

        // Wheels + prism facets.
        assert_eq!(g.wheel("ColorWheel 1").unwrap().slots.len(), 7);
        assert_eq!(g.wheel("GoboWheel 1").unwrap().slots.len(), 7);
        let p1 = g.wheel("Prism1").unwrap();
        assert_eq!(p1.slots[1].facets.len(), 5, "5-facet circular prism");
        let p2 = g.wheel("Prism2").unwrap();
        assert_eq!(p2.slots[1].facets.len(), 4, "4-facet linear prism");

        eprintln!(
            "Khamsin OK: {} wheels, beam {}°/{}° field, {}K {} {}lm; zoom {:?}",
            g.wheels.len(),
            g.beam.beam_angle,
            g.beam.field_angle,
            g.beam.color_temp,
            g.beam.lamp_type,
            g.beam.luminous_flux,
            zoom,
        );
    }

    /// Load a GDTF that ships inside the Basic Festival test MVR.
    fn load_from_festival(entry: &str) -> Option<GdtfFixture> {
        let path = format!(
            "{}/Downloads/Basic Festival/Basic Festival.mvr",
            std::env::var("HOME").unwrap_or_default()
        );
        let bytes = std::fs::read(&path).ok()?;
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.as_slice())).ok()?;
        let mut f = zip.by_name(entry).ok()?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        GdtfFixture::load_bytes(&buf).ok()
    }

    /// Multi-emitter extraction + DMX instancing against the Robe Spiider
    /// (18 pixels + flower; per-pixel RGBW via GeometryReference Breaks).
    /// Skips silently if the test MVR isn't present on this machine.
    #[test]
    fn spiider_multi_emitter() {
        let Some(g) = load_from_festival("Robe Lighting@Robin Spiider.gdtf") else {
            eprintln!("skip: Basic Festival MVR not found");
            return;
        };

        // Mode 8 - Pixel RGBW: 19 lens pixels + the flower overlay emitter.
        let (mi, mode) = g
            .modes
            .iter()
            .enumerate()
            .find(|(_, m)| m.name.starts_with("Mode 8"))
            .expect("mode 8");
        assert_eq!(mode.emitters.len(), 20, "19 pixels + flower");
        let washes = mode.emitters.iter().filter(|e| e.beam.beam_type == "Wash").count();
        assert_eq!(washes, 20, "all Spiider emitters are Wash type");
        // The flower emitter (inside the head, firing through the centre pixel)
        // merges into that front pixel; the 19 visible pixels stand alone.
        let merged: Vec<&EmitterDef> = mode.emitters.iter().filter(|e| e.merged_into.is_some()).collect();
        assert_eq!(merged.len(), 1, "exactly the flower merges");
        assert!(merged[0].name.contains("Flower"), "flower is the merged one: {}", merged[0].name);
        assert!((mode.emitters[0].beam.beam_radius - 0.0285).abs() < 1e-4);
        assert!((mode.emitters[0].beam.luminous_flux - 579.0).abs() < 0.5);

        // Per-instance address shift: Lens3 ColorAdd_R is authored at offset 63;
        // the 12 Zone3 instances sit at Break offsets 1,5,…,45 → 63,67,…,107.
        let lens3_r: Vec<u32> = mode
            .resolved
            .iter()
            .filter(|rc| {
                mode.channels[rc.channel].attribute == "ColorAdd_R"
                    && mode.channels[rc.channel].geometry == "Lens3"
            })
            .map(|rc| rc.offsets[0])
            .collect();
        assert_eq!(lens3_r.len(), 12, "12 Zone3 pixel instances");
        assert_eq!(lens3_r.iter().copied().min(), Some(63));
        assert_eq!(lens3_r.iter().copied().max(), Some(107));
        // Footprint covers the last pixel's W channel (66 + 45 − 1 = 110).
        assert_eq!(mode.footprint, 110, "instanced footprint, not naive max offset");
        // Each per-pixel channel instance covers exactly one cell.
        let one_cell = mode
            .resolved
            .iter()
            .filter(|rc| mode.channels[rc.channel].geometry == "Lens3")
            .all(|rc| rc.cells.len() == 1);
        assert!(one_cell, "pixel channels target exactly one cell");
        // Background color channels cover all 19 lens pixels (not the flower).
        let bg = mode
            .resolved
            .iter()
            .find(|rc| {
                mode.channels[rc.channel].geometry == "Background"
                    && mode.channels[rc.channel].attribute == "ColorAdd_R"
            })
            .expect("background red");
        assert_eq!(bg.cells.len(), 19);

        // Mode 7 - Pixel RGB uses DMXBreak="Overwrite" → the LAST Break row
        // (stride 3): Lens3 R at 56 + {1,4,…,34} − 1 → 56…89.
        let m7 = g
            .modes
            .iter()
            .find(|m| m.name.starts_with("Mode 7"))
            .expect("mode 7");
        let l3r: Vec<u32> = m7
            .resolved
            .iter()
            .filter(|rc| {
                m7.channels[rc.channel].attribute == "ColorAdd_R"
                    && m7.channels[rc.channel].geometry == "Lens3"
            })
            .map(|rc| rc.offsets[0])
            .collect();
        assert_eq!(l3r.iter().copied().min(), Some(56));
        assert_eq!(l3r.iter().copied().max(), Some(89));

        // Defaults (InitialFunction → ChannelFunction Default): background layer
        // full, pixel colors zero, master dimmer zero. (The flower layer also
        // defaults full — HTP with the white background makes that a no-op.)
        let default_of = |geom: &str, attr: &str| -> f32 {
            mode.channels
                .iter()
                .find(|c| c.geometry == geom && c.attribute == attr)
                .map(|c| c.default)
                .unwrap_or(f32::NAN)
        };
        assert!(default_of("Background", "ColorAdd_R") > 0.99);
        assert!(default_of("Background", "Dimmer") > 0.99);
        assert!(default_of("Lens3", "ColorAdd_R") < 0.01);
        assert!(default_of("Flower", "Dimmer") > 0.99);
        assert!(default_of("Head", "Dimmer") < 0.01);

        // Pixel positions: emitters spread across the head (Zone3 ring radius
        // ~0.1 m) once references are expanded — checked via the walk in
        // fixture_model (here just confirm the expansion produced Beam nodes).
        let root = g.root_for_mode(mi);
        fn count_beams(n: &Geometry) -> usize {
            n.children.iter().map(count_beams).sum::<usize>()
                + usize::from(n.kind == GeometryKind::Beam)
        }
        assert_eq!(count_beams(root), 20);

        eprintln!(
            "Spiider OK: {} emitters, footprint {}, lens3 R slots {:?}",
            mode.emitters.len(),
            mode.footprint,
            lens3_r
        );
    }

    /// The Astera PixelBar picks a different geometry root per DMX mode
    /// (4/8/16-pixel hardware variants of one fixture type).
    #[test]
    fn pixelbar_mode_roots() {
        let Some(g) = load_from_festival("Astera LED Technology@AX2-100 PixelBar.gdtf") else {
            eprintln!("skip: Basic Festival MVR not found");
            return;
        };
        assert!(g.roots.len() > 10, "many top-level roots, got {}", g.roots.len());
        // Every mode resolves a root and its pixel count matches the mode name.
        for (i, m) in g.modes.iter().enumerate() {
            let n = g.emitters(i).len();
            assert!(n >= 1, "mode {} '{}' has no emitters", i, m.name);
            let root = g.root_for_mode(i);
            assert_eq!(root.name, m.geometry, "mode root resolves by name");
        }
        let counts: Vec<usize> = (0..g.modes.len()).map(|i| g.emitters(i).len()).collect();
        assert!(counts.contains(&16), "a 16-pixel mode exists: {counts:?}");
        eprintln!("PixelBar OK: roots {}, per-mode emitters {:?}", g.roots.len(), counts);
    }
}
