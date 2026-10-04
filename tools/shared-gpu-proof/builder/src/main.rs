//! Compile only: no GPU context, mining work or network submission is created.
use std::{error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = root.join("artifacts/shared-gpu-proof");
    fs::create_dir_all(&output)?;
    // #### PR #22: the portable shared stages, one module.
    // PICKAXE_BUILD_SHARED_STAGES=debug adds a development-only arithmetic entry.
    if let Some(mode) = std::env::var_os("PICKAXE_BUILD_SHARED_STAGES") {
        let debug = mode == "debug";
        let result = spirv_builder::SpirvBuilder::new(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../stages"),
            "spirv-unknown-vulkan1.1",
        )
        .shader_crate_features(debug.then(|| "debug".to_string()))
        .build()?;
        let path = result.module.unwrap_single();
        let module = naga::front::spv::parse_u8_slice(&fs::read(path)?, &Default::default())?;
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)?;
        let directory = output.join(if debug { "stages-debug" } else { "stages" });
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("pickaxe_shared_stages.wgsl"),
            naga::back::wgsl::write_string(
                &module,
                &info,
                naga::back::wgsl::WriterFlags::EXPLICIT_TYPES,
            )?,
        )?;
        let (metal, _) =
            naga::back::msl::write_string(&module, &info, &Default::default(), &Default::default())?;
        fs::write(directory.join("pickaxe_shared_stages.metal"), metal)?;
        fs::copy(path, directory.join("pickaxe_shared_stages.spv"))?;
        println!("Shared stages generated in {}", directory.display());
        return Ok(());
    }
    if std::env::var_os("PICKAXE_BUILD_SHARED_FILTER").is_some() {
        let result = spirv_builder::SpirvBuilder::new(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../filter"),
            "spirv-unknown-vulkan1.1",
        )
        .multimodule(true)
        .build()?;
        let spirv_builder::ModuleResult::MultiModule(modules) = result.module else {
            return Err("Expected one portable module per layout".into());
        };
        let directory = output.join("filter");
        if modules.len() != 17 {
            return Err("Expected exactly 17 layout kernels".into());
        }
        fs::create_dir_all(&directory)?;
        for (name, path) in modules {
            let name = name.rsplit("::").next().ok_or("Missing kernel name")?;
            let module = naga::front::spv::parse_u8_slice(&fs::read(&path)?, &Default::default())?;
            let info = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)?;
            fs::write(
                directory.join(format!("{name}.wgsl")),
                naga::back::wgsl::write_string(
                    &module,
                    &info,
                    naga::back::wgsl::WriterFlags::EXPLICIT_TYPES,
                )?,
            )?;
            let (metal, _) = naga::back::msl::write_string(
                &module,
                &info,
                &Default::default(),
                &Default::default(),
            )?;
            fs::write(directory.join(format!("{name}.metal")), metal)?;
            fs::copy(path, directory.join(format!("{name}.spv")))?;
        }
        println!("Shared T2 filters generated in {}", directory.display());
        return Ok(());
    }
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
