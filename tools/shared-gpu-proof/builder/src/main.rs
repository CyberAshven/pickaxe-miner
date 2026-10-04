//! Compile only: no GPU context, mining work or network submission is created.
use std::{error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = root.join("artifacts/shared-gpu-proof");
    fs::create_dir_all(&output)?;
    let result = spirv_builder::SpirvBuilder::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../kernel"),
        "spirv-unknown-vulkan1.1",
    )
    .build()?;
    let path = result.module.unwrap_single();
    fs::copy(path, output.join("shared-t2-hash.spv"))?;
    let bytes = fs::read(path)?;
    let module = naga::front::spv::parse_u8_slice(&bytes, &Default::default())?;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)?;
    let wgsl = naga::back::wgsl::write_string(
        &module,
        &info,
        naga::back::wgsl::WriterFlags::EXPLICIT_TYPES,
    )?;
    fs::write(output.join("shared-t2-hash.wgsl"), wgsl)?;
    let (msl, _) =
        naga::back::msl::write_string(&module, &info, &Default::default(), &Default::default())?;
    fs::write(output.join("shared-t2-hash.metal"), msl)?;
    println!(
        "SPIR-V, WGSL and Metal source generated in {}",
        output.display()
    );
    Ok(())
}
