//! Write `frontend/src/api/generated.ts` from the registry's OpenAPI document.
//!
//! `crates/loom/build.rs` does this on every build. This binary is the same
//! render for a frontend-only workflow that has not compiled `loom` yet.

fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../loom/frontend/src/api/generated.ts");
    std::fs::create_dir_all(path.parent().expect("output directory")).expect("create dir");
    std::fs::write(&path, weaver_api::typescript::module()).expect("write generated module");
    println!("wrote {}", path.display());
}
