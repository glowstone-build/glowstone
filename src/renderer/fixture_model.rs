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
                let p = world.transform_point3(Vec3::from(positions[i]));
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

/// The assembled fixture: parts to draw plus the beam's world frame.
pub struct Assembly {
    pub parts: Vec<PartDraw>,
    /// Beam frame, if the fixture has a Beam geometry.
    pub beam: Option<BeamFrame>,
}

/// Walk the geometry tree with pan/tilt applied, in world space (`root` already
/// maps GDTF -> world and places the fixture).
pub fn assemble(fixture: &GdtfFixture, root: Mat4, pan_deg: f32, tilt_deg: f32) -> Assembly {
    let pan_geom = fixture.geometry_for_attribute("Pan").map(str::to_string);
    let tilt_geom = fixture.geometry_for_attribute("Tilt").map(str::to_string);

    let mut parts = Vec::new();
    let mut beam: Option<BeamFrame> = None;
    walk(
        &fixture.geometry,
        root,
        pan_deg,
        tilt_deg,
        pan_geom.as_deref(),
        tilt_geom.as_deref(),
        &mut parts,
        &mut beam,
    );
    Assembly { parts, beam }
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
    beam: &mut Option<BeamFrame>,
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
        *beam = Some(BeamFrame { origin, dir, right, up });
    }

    for child in &node.children {
        walk(child, world, pan, tilt, pan_geom, tilt_geom, parts, beam);
    }
}
