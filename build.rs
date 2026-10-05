// #### PR #22: do not silently ship stale portable code after a native Rust edit.
// CI additionally rebuilds these artifacts with the pinned SPIR-V compiler.
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_SHARED_RUST_T2");
    if env::var_os("CARGO_FEATURE_SHARED_RUST_T2").is_none() {
        return;
    }
    // (generated directory, shaders, shared sources) per portable artifact set.
    verify("reference/shared-t2", 17, 10);
    verify("reference/shared-stages", 2, 15);
}

fn verify(directory: &str, expected_shaders: usize, expected_sources: usize) {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let manifest = format!("{directory}/SHA256SUMS");
    println!("cargo:rerun-if-changed={manifest}");
    let contents = fs::read_to_string(root.join(&manifest))
        .expect("Generate portable shaders: python tools/shared-gpu-proof/sync_filter.py --write");
    let mut shaders = 0;
    let mut sources = 0;
    for line in contents.lines() {
        let (expected, path) = line.split_once("  ").expect("Malformed shader provenance");
        println!("cargo:rerun-if-changed={path}");
        let source =
            fs::read_to_string(root.join(path)).expect("Missing shared GPU source/artifact");
        let actual: String = Sha256::digest(source.replace("\r\n", "\n").as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(actual, expected,
            "Stale shared GPU source/artifact: {path}. Run python tools/shared-gpu-proof/sync_filter.py --write");
        if path.starts_with(&format!("{directory}/")) {
            shaders += 1;
        } else {
            sources += 1;
        }
    }
    assert_eq!(
        shaders, expected_shaders,
        "Incomplete portable shader manifest"
    );
    assert_eq!(
        sources, expected_sources,
        "Incomplete shared Rust source manifest"
    );
}
