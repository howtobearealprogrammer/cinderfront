//! Bakes every runtime asset (models, icons, font) into the binary so the game
//! ships as a single file. Generates `$OUT_DIR/assets.rs`: a list of
//! (path relative to assets/, bytes).

use std::path::Path;

fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).expect("read assets").flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, root, out);
        } else if matches!(p.extension().and_then(|e| e.to_str()), Some("glb" | "png" | "ttf")) {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let abs = p.canonicalize().unwrap();
            out.push(format!("    ({rel:?}, include_bytes!({:?}) as &[u8]),", abs.to_string_lossy()));
        }
    }
}

fn main() {
    let root = Path::new("assets");
    println!("cargo:rerun-if-changed=assets");
    let mut items = Vec::new();
    walk(root, root, &mut items);
    let code = format!("pub const ASSETS: &[(&str, &[u8])] = &[\n{}\n];\n", items.join("\n"));
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("assets.rs"), code).unwrap();
}
