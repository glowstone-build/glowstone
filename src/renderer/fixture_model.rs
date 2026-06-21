//! Loading and assembling GDTF fixture 3D models.
//!
//! GDTF ships glTF (GLB) meshes per geometry part (base / yoke / head). We bake
//! each GLB into world-space triangles in its own local frame, then walk the
//! GDTF geometry hierarchy each frame — applying pan to the yoke axis and tilt
//! to the head axis — to place every part and to derive the beam's origin and
//! direction.
//!
//! Coordinate conversion: GDTF is +Z-up; the app world is +Y-up. The caller
//! passes a root transform that already maps GDTF -> world and places the
//! fixture; everything below stays in GDTF space until that root applies.

use std::f32::consts::FRAC_PI_2;

use glam::{Mat3, Mat4, Vec3};

use super::mesh::MeshVertex;
use crate::gdtf::{GdtfFixture, Geometry, GeometryKind};

/// Bake an MVR/GDTF model file into triangles, dispatching on extension: `.3ds`
/// (3D Studio binary, common for MVR stage/rigging geometry) goes to
/// [`load_3ds`]; everything else (`.glb`/`.gltf`) to [`load_glb`].
///
/// Note the two formats use different up-axes — glTF is +Y-up (assimp export),
/// `.3ds` is natively +Z-up (the MVR geometry frame). The caller picks the right
/// model→geometry rotation per file via [`model_yup_flip`].
pub fn load_model(file: &str, bytes: &[u8]) -> Vec<MeshVertex> {
    if file.to_ascii_lowercase().ends_with(".3ds") {
        load_3ds(bytes)
    } else {
        load_glb(bytes)
    }
}

/// Whether a model file's vertices need the +Y-up → +Z-up rotation before the
/// MVR placement matrix. glTF needs it; native-Z-up `.3ds` does not.
pub fn model_needs_yup_flip(file: &str) -> bool {
    !file.to_ascii_lowercase().ends_with(".3ds")
}

/// Bake a 3D Studio (`.3ds`) binary model into non-indexed triangles with flat
/// per-face normals, in the file's native (+Z-up) frame. 3DS is a little-endian,
/// chunk-tree format; we walk MAIN → EDIT → OBJECT → TRIMESH and read each
/// trimesh's vertex list (0x4110) + face list (0x4120). All reads are bounds-
/// checked (the bytes are untrusted MVR archive content).
pub fn load_3ds(bytes: &[u8]) -> Vec<MeshVertex> {
    let mut out = Vec::new();
    scan_3ds(bytes, &mut out, 0);
    out
}

fn le_u16(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn le_u32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn le_f32(b: &[u8], o: usize) -> Option<f32> {
    b.get(o..o + 4).map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Recursively scan a region of 3DS chunks, emitting triangles for every
/// trimesh. `depth` guards against pathological nesting.
fn scan_3ds(buf: &[u8], out: &mut Vec<MeshVertex>, depth: u32) {
    if depth > 32 {
        return;
    }
    let mut pos = 0usize;
    while let (Some(id), Some(len)) = (le_u16(buf, pos), le_u32(buf, pos + 2)) {
        let len = len as usize;
        if len < 6 || pos + len > buf.len() {
            break; // malformed/truncated chunk — stop this level
        }
        let body = &buf[pos + 6..pos + len];
        match id {
            0x4D4D | 0x3D3D => scan_3ds(body, out, depth + 1), // MAIN3DS / EDIT3DS
            0x4000 => {
                // EDIT_OBJECT: null-terminated name, then sub-chunks.
                let name_end = body.iter().position(|&b| b == 0).map(|p| p + 1).unwrap_or(body.len());
                scan_3ds(&body[name_end.min(body.len())..], out, depth + 1);
            }
            0x4100 => emit_3ds_trimesh(body, out), // OBJ_TRIMESH
            _ => {}
        }
        pos += len;
    }
}

/// Read a TRIMESH chunk's vertex + face lists and append its triangles.
fn emit_3ds_trimesh(body: &[u8], out: &mut Vec<MeshVertex>) {
    let mut verts: Vec<Vec3> = Vec::new();
    let mut faces: Vec<[u16; 3]> = Vec::new();
    let mut pos = 0usize;
    while let (Some(id), Some(len)) = (le_u16(body, pos), le_u32(body, pos + 2)) {
        let len = len as usize;
        if len < 6 || pos + len > body.len() {
            break;
        }
        let sub = &body[pos + 6..pos + len];
        match id {
            0x4110 => {
                // TRI_VERTEXL: u16 count, then count × (3 × f32).
                if let Some(n) = le_u16(sub, 0) {
                    for i in 0..n as usize {
                        let o = 2 + i * 12;
                        match (le_f32(sub, o), le_f32(sub, o + 4), le_f32(sub, o + 8)) {
                            (Some(x), Some(y), Some(z)) => verts.push(Vec3::new(x, y, z)),
                            _ => break,
                        }
                    }
                }
            }
            0x4120 => {
                // TRI_FACEL: u16 count, then count × (3 × u16 index + u16 flags).
                // Trailing material/smoothing sub-chunks are ignored (skipped by len).
                if let Some(n) = le_u16(sub, 0) {
                    for i in 0..n as usize {
                        let o = 2 + i * 8;
                        match (le_u16(sub, o), le_u16(sub, o + 2), le_u16(sub, o + 4)) {
                            (Some(a), Some(b), Some(c)) => faces.push([a, b, c]),
                            _ => break,
                        }
                    }
                }
            }
            _ => {}
        }
        pos += len;
    }
    // MVR authors `.3ds` geometry in MILLIMETRES (the MVR coordinate unit), while
    // the app world + the GLB path + the placement matrix are in metres — so scale
    // mm → m here, else the geometry renders ~1000× too large.
    const MM_TO_M: f32 = 0.001;
    for f in &faces {
        if let (Some(&v0), Some(&v1), Some(&v2)) =
            (verts.get(f[0] as usize), verts.get(f[1] as usize), verts.get(f[2] as usize))
        {
            let n = (v1 - v0).cross(v2 - v0).normalize_or_zero();
            for &p in &[v0, v1, v2] {
                out.push(MeshVertex {
                    position: (p * MM_TO_M).to_array(),
                    normal: n.to_array(),
                    emissive: 0.0,
                });
            }
        }
    }
}

/// Bake a GLB into non-indexed triangles (positions + normals), applying the
/// glTF node hierarchy. Returns vertices in the GLB's own space.
///
/// Uses the non-validating loader because GDTF GLBs often declare required
/// extensions (e.g. `KHR_texture_transform`) that only affect material UVs —
/// irrelevant here since we read positions and normals from the binary blob.
pub fn load_glb(bytes: &[u8]) -> Vec<MeshVertex> {
    let glb = match gltf::Gltf::from_slice_without_validation(bytes) {
        Ok(g) => g,
        Err(e) => {
            log::warn!("gltf parse failed: {e}");
            return Vec::new();
        }
    };
    let blob = glb.blob.as_deref();
    let mut out = Vec::new();
    for scene in glb.document.scenes() {
        for node in scene.nodes() {
            collect_node(&node, Mat4::IDENTITY, blob, &mut out);
        }
    }
    out
}

fn collect_node(node: &gltf::Node, parent: Mat4, blob: Option<&[u8]>, out: &mut Vec<MeshVertex>) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;
    let normal_mat = Mat3::from_mat4(world).inverse().transpose();

    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| if b.index() == 0 { blob } else { None });
            let positions: Vec<[f32; 3]> = match reader.read_positions() {
                Some(it) => it.collect(),
                None => continue,
            };
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|it| it.collect())
                .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; positions.len()]);
            let indices: Vec<u32> = reader
                .read_indices()
                .map(|it| it.into_u32().collect())
                .unwrap_or_else(|| (0..positions.len() as u32).collect());

            for &i in &indices {
                let i = i as usize;
                // Bounds-guard the index buffer: a malformed GLB (now reachable
                // via untrusted MVR/GDTF import) can reference out-of-range verts.
                let Some(&pos) = positions.get(i) else { continue };
                let p = world.transform_point3(Vec3::from(pos));
                let n = (normal_mat * Vec3::from(normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0])))
                    .normalize_or_zero();
                out.push(MeshVertex {
                    position: p.to_array(),
                    normal: n.to_array(),
                    emissive: 0.0,
                });
            }
        }
    }

    for child in node.children() {
        collect_node(&child, world, blob, out);
    }
}

/// One part to draw: the model name (mesh key) and its model->world transform.
pub struct PartDraw {
    pub model: String,
    pub world: Mat4,
}

/// The beam's world-space frame: where it exits, the direction it points, and a
/// stable lens-plane basis (`right`/`up`) for projecting gobo cookies. Taken
/// from the Beam geometry's articulated world matrix so it never flips (unlike a
/// basis derived from the direction alone when the head points straight down).
#[derive(Clone, Copy)]
pub struct BeamFrame {
    pub origin: Vec3,
    pub dir: Vec3,
    pub right: Vec3,
    pub up: Vec3,
}

/// The assembled fixture: parts to draw plus every emitter's world frame.
pub struct Assembly {
    pub parts: Vec<PartDraw>,
    /// One frame per emitter, in the SAME depth-first order as the mode's
    /// [`emitters`](crate::gdtf::DmxMode::emitters) list (index-aligned).
    pub beams: Vec<BeamFrame>,
}

/// Walk the mode's expanded geometry tree with pan/tilt applied, in world space
/// (`root` already maps GDTF -> world and places the fixture).
pub fn assemble(
    fixture: &GdtfFixture,
    mode_index: usize,
    root: Mat4,
    pan_deg: f32,
    tilt_deg: f32,
) -> Assembly {
    let pan_geom = fixture.geometry_for_attribute("Pan").map(str::to_string);
    let tilt_geom = fixture.geometry_for_attribute("Tilt").map(str::to_string);

    let mut parts = Vec::new();
    let mut beams = Vec::new();
    walk(
        fixture.root_for_mode(mode_index),
        root,
        pan_deg,
        tilt_deg,
        pan_geom.as_deref(),
        tilt_geom.as_deref(),
        &mut parts,
        &mut beams,
    );
    Assembly { parts, beams }
}

#[allow(clippy::too_many_arguments)]
fn walk(
    node: &Geometry,
    parent: Mat4,
    pan: f32,
    tilt: f32,
    pan_geom: Option<&str>,
    tilt_geom: Option<&str>,
    parts: &mut Vec<PartDraw>,
    beams: &mut Vec<BeamFrame>,
) {
    let mut local = node.matrix;
    // GDTF axes rotate about their local axis: pan about +Z (up), tilt about +X.
    if Some(node.name.as_str()) == pan_geom {
        local *= Mat4::from_rotation_z(pan.to_radians());
    }
    if Some(node.name.as_str()) == tilt_geom {
        local *= Mat4::from_rotation_x(tilt.to_radians());
    }
    let world = parent * local;

    if let Some(model) = &node.model {
        // The glTF meshes are authored +Y-up; the GDTF geometry frame is +Z-up.
        // Rotate each part's mesh from Y-up into the geometry's Z-up frame
        // before its placement matrix applies.
        parts.push(PartDraw {
            model: model.clone(),
            world: world * Mat4::from_rotation_x(FRAC_PI_2),
        });
    }
    if node.kind == GeometryKind::Beam {
        let origin = world.transform_point3(Vec3::ZERO);
        // The beam emits along the geometry's local -Z (GDTF "down"); local X/Y
        // are the stable cookie basis (carried through pan/tilt by `world`).
        let dir = world.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
        let right = world.transform_vector3(Vec3::X).normalize_or_zero();
        let up = world.transform_vector3(Vec3::Y).normalize_or_zero();
        beams.push(BeamFrame { origin, dir, right, up });
    }

    for child in &node.children {
        walk(child, world, pan, tilt, pan_geom, tilt_geom, parts, beams);
    }
}

#[cfg(test)]
mod tests_3ds {
    use super::*;

    /// Wrap a body in a 3DS chunk header (id + total length).
    fn chunk(id: u16, body: &[u8]) -> Vec<u8> {
        let len = 6 + body.len() as u32;
        let mut v = id.to_le_bytes().to_vec();
        v.extend(len.to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn parses_minimal_triangle() {
        let mut verts = (3u16).to_le_bytes().to_vec();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for c in p {
                verts.extend(c.to_le_bytes());
            }
        }
        let mut faces = (1u16).to_le_bytes().to_vec();
        for x in [0u16, 1, 2, 0] {
            faces.extend(x.to_le_bytes());
        }
        let trimesh = [chunk(0x4110, &verts), chunk(0x4120, &faces)].concat();
        let mut objbody = b"obj\0".to_vec();
        objbody.extend(chunk(0x4100, &trimesh));
        let main = chunk(0x4D4D, &chunk(0x3D3D, &chunk(0x4000, &objbody)));

        let out = load_3ds(&main);
        assert_eq!(out.len(), 3, "one triangle = three vertices");
        // Triangle lies in the XY plane → flat normal along Z.
        assert!(out[0].normal[2].abs() > 0.9, "flat normal should point along +/-Z");
        // Vertices are scaled mm → m (×0.001).
        assert!((out[1].position[0] - 0.001).abs() < 1e-6);
    }

    #[test]
    fn malformed_input_never_panics() {
        let _ = load_3ds(&[]);
        let _ = load_3ds(&[0x4d, 0x4d, 0xff, 0xff, 0xff, 0xff]); // oversized length
        let _ = load_3ds(&[0x4d, 0x4d, 0x06, 0x00, 0x00, 0x00]); // empty main
        let _ = load_3ds(&vec![0u8; 256]);
        let _ = load_3ds(&(0..=255u8).cycle().take(2048).collect::<Vec<_>>());
        // load_model dispatch on extension
        assert!(load_model("x.3ds", &[]).is_empty());
        assert!(load_model("x.glb", &[]).is_empty());
        assert!(model_needs_yup_flip("a.glb"));
        assert!(!model_needs_yup_flip("a.3ds"));
        assert!(!model_needs_yup_flip("A.3DS"));
    }
}
