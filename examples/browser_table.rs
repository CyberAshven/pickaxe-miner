//! Export the existing, hash-verified GPU table for a static browser bundle.
fn main() -> Result<(), String> {
    let output = std::env::args().nth(1).ok_or("expected output path")?;
    let (table, _) = pickaxe_miner::m29_table::load_or_generate_m29_g16()?;
    std::fs::write(output, table).map_err(|error| error.to_string())
}
