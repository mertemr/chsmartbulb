use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The web interface, embedded so the app can serve it to the network.
fn embed_web_interface() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/bundle");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let mut code = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for path in files {
        let name = path.strip_prefix(&root).expect("under root").to_string_lossy().replace('\\', "/");
        let absolute = path.canonicalize().expect("readable file");
        writeln!(code, "    ({name:?}, include_bytes!({:?})),", absolute.to_string_lossy()).unwrap();
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo")).join("webui.rs");
    std::fs::write(out, code).expect("OUT_DIR is writable");
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn main() {
    embed_web_interface();
    tauri_build::build()
}
