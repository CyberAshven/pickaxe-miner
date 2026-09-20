//! Clap CLI entry (DoD): devices / repl. mine/benchmark come next.

use crate::backend::{self, BackendKind};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "pickaxe", about = "Pickaxe Miner — GPU-only PHOTON miner")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List GPUs / backend availability
    Devices {
        #[arg(long, default_value = "auto")]
        backend: String,
    },
    /// Interactive REPL shell
    Repl,
}

pub fn run_cli(run_repl: impl FnOnce()) {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Commands::Devices { backend }) => {
            let kind = match BackendKind::parse(&backend) {
                Ok(k) => k,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            };
            if let Err(e) = backend::print_devices(kind) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some(Commands::Repl) | None => run_repl(),
    }
}
