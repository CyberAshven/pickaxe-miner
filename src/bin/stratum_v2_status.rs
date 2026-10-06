//! Temporary CLI entry until `src/main.rs` receives the dispatch in `main.rs.diff`.
//!
//! ```text
//! cargo run --bin stratum_v2_status
//! ```
//!
//! Prefer `pickaxe stratum-v2 status` once the main dispatch lands (feature
//! `stratum-v2` enables the clap subcommand).

fn main() {
    print!("{}", pickaxe_miner::stratum_v2::status_report());
}
