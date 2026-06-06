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
    pub geometry: Geometry,
    pub modes: Vec<DmxMode>,
    /// Beam cone angle in degrees (from the Beam geometry), if present. Kept as
    /// a top-level field for back-compat; the full optics live in [`beam`].
    pub beam_angle: f32,
    /// Physical source/optics parameters from the `Beam` geometry.
    pub beam: BeamData,
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
}

#[derive(Clone)]
pub struct DmxMode {
    pub name: String,
    pub channels: Vec<DmxChannel>,
    /// Number of DMX slots the mode occupies (max byte offset).
    pub footprint: u32,
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct DmxChannel {
    pub geometry: String,
    pub offsets: Vec<u32>,
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
        Self::load_bytes(&bytes)
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

        // --- geometry tree ---
        let geometry = ft
            .children()
            .find(|n| n.has_tag_name("Geometries"))
            .and_then(|g| g.children().find(|c| c.is_element()))
            .map(|root| parse_geometry(&root))
            .ok_or("no geometry")?;

        let beam = doc
            .descendants()
            .find(|n| n.has_tag_name("Beam"))
            .map(|b| {
                let f = |k: &str, d: f32| b.attribute(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
                let s = |k: &str, d: &str| b.attribute(k).unwrap_or(d).to_string();
                let beam_angle = f("BeamAngle", 15.0);
                BeamData {
                    beam_angle,
                    field_angle: f("FieldAngle", beam_angle),
                    beam_radius: f("BeamRadius", 0.08),
                    beam_type: s("BeamType", "Spot"),
                    color_temp: f("ColorTemperature", 6500.0),
                    cri: f("ColorRenderingIndex", 90.0),
                    luminous_flux: f("LuminousFlux", 10000.0),
                    lamp_type: s("LampType", "LED"),
                    throw_ratio: f("ThrowRatio", 1.0),
                    power: f("PowerConsumption", 300.0),
                }
            })
            .unwrap_or_default();
        let beam_angle = beam.beam_angle;

        // --- DMX modes ---
        let mut modes = Vec::new();
        if let Some(dm) = ft.children().find(|n| n.has_tag_name("DMXModes")) {
            for mode in dm.children().filter(|n| n.has_tag_name("DMXMode")) {
                let mut channels = Vec::new();
                let mut footprint = 0u32;
                if let Some(chs) = mode.children().find(|n| n.has_tag_name("DMXChannels")) {
                    for ch in chs.children().filter(|n| n.has_tag_name("DMXChannel")) {
                        let offsets: Vec<u32> = ch
                            .attribute("Offset")
                            .unwrap_or("")
                            .split(',')
                            .filter_map(|s| s.trim().parse::<u32>().ok())
                            .collect();
                        footprint = footprint.max(offsets.iter().copied().max().unwrap_or(0));
                        let resolution = offsets.len().max(1) as u8;

                        // Every channel function across every logical channel.
                        let mut functions = Vec::new();
                        for lc in ch.children().filter(|n| n.has_tag_name("LogicalChannel")) {
                            let lc_attr = lc.attribute("Attribute").unwrap_or("");
                            for cf in lc.children().filter(|c| c.has_tag_name("ChannelFunction")) {
                                let pf = cf.attribute("PhysicalFrom").and_then(|v| v.parse().ok());
                                let pt = cf.attribute("PhysicalTo").and_then(|v| v.parse().ok());
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
                            attribute,
                            function,
                            sets,
                            resolution,
                            functions,
                        });
                    }
                }
                modes.push(DmxMode {
                    name: attr(&mode, "Name"),
                    channels,
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
            modes,
            beam_angle,
            beam,
        })
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
    Geometry {
        name: node.attribute("Name").unwrap_or("").to_string(),
        kind,
        model: node.attribute("Model").map(|s| s.to_string()),
        matrix,
        children,
    }
}

/// Parse a GDTF matrix `{a,b,c,d}{e,f,g,h}{...}{...}` (4 rows, column-vector
/// convention) into a glam `Mat4`.
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
    // `nums` is row-major (M[row][col]); glam is column-major.
    Some(Mat4::from_cols_array(&[
        nums[0], nums[4], nums[8], nums[12], // col 0
        nums[1], nums[5], nums[9], nums[13], // col 1
        nums[2], nums[6], nums[10], nums[14], // col 2
        nums[3], nums[7], nums[11], nums[15], // col 3
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

/// Parse a GDTF CIE `x,y,Y` color string into approximate linear RGB.
fn parse_cie_xyy(s: &str) -> Option<[f32; 3]> {
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
    let mut buf = Vec::with_capacity(f.size() as usize);
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
}
