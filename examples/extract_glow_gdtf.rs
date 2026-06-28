//! Throwaway: pull the bundled `.gdtf` archive(s) out of a `.glow` show file and
//! dump each one's `description.xml`. Usage:
//!   cargo run --example extract_glow_gdtf -- <show.glow> <out_dir>

use std::collections::HashMap;
use std::io::Read;

use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Extract {
    // Field name must match `project::Project::gdtf_assets`. rmp-serde decodes a
    // named map and ignores every other field, so we only declare this one.
    // `Vec<u8>` (NOT serde_bytes) because the project serialises it as a normal
    // seq via `to_vec_named`.
    gdtf_assets: HashMap<String, Vec<u8>>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let show = &args[1];
    let out_dir = args.get(2).cloned().unwrap_or_else(|| "/tmp/glow_gdtf".into());
    std::fs::create_dir_all(&out_dir).unwrap();

    let bytes = std::fs::read(show).unwrap();
    assert!(bytes.starts_with(b"GLOW\0"), "not a .glow file");
    let head = 5 + 4; // MAGIC + u32 version
    let ex: Extract = rmp_serde::from_slice(&bytes[head..]).expect("decode .glow body");

    println!("gdtf_assets: {} archive(s)", ex.gdtf_assets.len());
    for (spec, data) in &ex.gdtf_assets {
        let safe = spec.replace('/', "_");
        let gdtf_path = format!("{out_dir}/{safe}");
        std::fs::write(&gdtf_path, data).unwrap();
        println!("\n=== {spec} ({} bytes) -> {gdtf_path}", data.len());

        // List entries + dump description.xml.
        let mut zip =
            zip::ZipArchive::new(std::io::Cursor::new(data.as_slice())).expect("open zip");
        let names: Vec<String> = (0..zip.len())
            .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
            .collect();
        println!("entries ({}):", names.len());
        for n in &names {
            println!("  {n}");
        }
        if let Ok(mut f) = zip.by_name("description.xml") {
            let mut xml = String::new();
            f.read_to_string(&mut xml).unwrap();
            let xml_path = format!("{out_dir}/{safe}.description.xml");
            std::fs::write(&xml_path, &xml).unwrap();
            println!("description.xml -> {xml_path} ({} bytes)", xml.len());
        }
    }
}
