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
    /// Beam cone angle in degrees (from the Beam geometry), if present.
    pub beam_angle: f32,
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
    pub attribute: String,
    pub function: String,
    pub sets: Vec<String>,
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
                    slots.push(WheelSlot {
                        name: attr(&s, "Name"),
                        color,
                        media,
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

        let beam_angle = doc
            .descendants()
            .find(|n| n.has_tag_name("Beam"))
            .and_then(|b| b.attribute("BeamAngle"))
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(15.0);

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
        })
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
