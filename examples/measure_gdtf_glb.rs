//! Throwaway: for each GDTF Model with a glTF file, bake the GLB and print its
//! actual bounding box vs the declared Width/Height/Length — to find a part whose
//! mesh was exported at the wrong scale.
//!   cargo run --example measure_gdtf_glb -- <fixture.gdtf>

use std::io::Read;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.as_slice())).unwrap();
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();

    let mut xml = String::new();
    zip.by_name("description.xml").unwrap().read_to_string(&mut xml).unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();

    for m in doc.descendants().filter(|n| n.has_tag_name("Model")) {
        let name = m.attribute("Name").unwrap_or("");
        let file = m.attribute("File").unwrap_or("");
        let dw: f32 = m.attribute("Width").and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let dh: f32 = m.attribute("Height").and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let dl: f32 = m.attribute("Length").and_then(|v| v.parse().ok()).unwrap_or(0.0);
        if file.is_empty() {
            continue;
        }
        // Resolve the GLB (prefer high, like the renderer).
        let want_hi = format!("models/gltf_high/{file}.glb");
        let want_lo = format!("models/gltf/{file}.glb");
        let entry = names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(&want_hi))
            .or_else(|| names.iter().find(|n| n.eq_ignore_ascii_case(&want_lo)));
        let Some(entry) = entry.cloned() else {
            println!("{name:24} file={file:24} (no GLB in archive)");
            continue;
        };
        let mut buf = Vec::new();
        zip.by_name(&entry).unwrap().read_to_end(&mut buf).unwrap();
        let (min, max) = glb_bbox(&buf);
        let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        let decl_max = dw.max(dh).max(dl).max(1e-6);
        let baked_max = size[0].max(size[1]).max(size[2]);
        let ratio = baked_max / decl_max;
        let flag = if ratio > 3.0 || ratio < 0.33 { "  <<< OUT OF SCALE" } else { "" };
        println!(
            "{name:24} decl {:.3}x{:.3}x{:.3}  baked {:.3}x{:.3}x{:.3}  ratio {:.1}x{}",
            dw, dh, dl, size[0], size[1], size[2], ratio, flag
        );
    }
}

fn glb_bbox(bytes: &[u8]) -> ([f32; 3], [f32; 3]) {
    let glb = gltf::Gltf::from_slice_without_validation(bytes).unwrap();
    let blob = glb.blob.as_deref();
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    // Walk node hierarchy to apply transforms (matches the renderer's collect_node).
    fn node(n: &gltf::Node, parent: [[f32; 4]; 4], blob: Option<&[u8]>, mn: &mut [f32; 3], mx: &mut [f32; 3]) {
        let local = n.transform().matrix();
        let world = matmul(parent, local);
        if let Some(mesh) = n.mesh() {
            for prim in mesh.primitives() {
                let reader = prim.reader(|b| if b.index() == 0 { blob } else { None });
                if let Some(it) = reader.read_positions() {
                    for p in it {
                        let w = transform(world, p);
                        for k in 0..3 {
                            mn[k] = mn[k].min(w[k]);
                            mx[k] = mx[k].max(w[k]);
                        }
                    }
                }
            }
        }
        for c in n.children() {
            node(&c, world, blob, mn, mx);
        }
    }
    for scene in glb.document.scenes() {
        for n in scene.nodes() {
            node(&n, identity(), blob, &mut min, &mut max);
        }
    }
    (min, max)
}

fn identity() -> [[f32; 4]; 4] {
    let mut m = [[0.0; 4]; 4];
    for i in 0..4 {
        m[i][i] = 1.0;
    }
    m
}
fn matmul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut r = [[0.0; 4]; 4];
    for c in 0..4 {
        for row in 0..4 {
            for k in 0..4 {
                r[c][row] += a[k][row] * b[c][k];
            }
        }
    }
    r
}
fn transform(m: [[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}
