#[path = "protocol/profile.rs"]
mod profile;

fn main() {
    println!("cargo:rerun-if-changed=protocol/photon.json");
    println!("cargo:rerun-if-changed=protocol/profile.rs");
    let source = std::fs::read_to_string("protocol/photon.json").expect("read protocol profile");
    let constants = profile::constants(&source).expect("invalid protocol profile");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("photon_profile.rs"), constants).expect("write protocol constants");
}
