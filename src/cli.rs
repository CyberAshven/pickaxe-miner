//! Single Clap startup parser for Pickaxe.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "pickaxe", about = "Pickaxe Miner - GPU-only PHOTON miner")]
pub struct Cli {
    #[arg(long, global = true, default_value = "auto", value_parser = ["auto", "cuda", "hip", "wgpu"])]
    pub backend: String,

    #[arg(long, global = true)]
    pub device: Option<u32>,

    #[arg(long, global = true, default_value_t = 100, value_parser = clap::value_parser!(u8).range(10..=100))]
    pub intensity: u8,

    #[arg(long, global = true)]
    pub address: Option<String>,

    #[arg(long = "node-rpc", alias = "node", global = true)]
    pub node_rpc: Option<String>,

    #[arg(long, global = true)]
    pub fulcrum: Option<String>,

    #[arg(long, global = true, value_parser = ["node", "fulcrum"])]
    pub source: Option<String>,

    #[arg(long, global = true)]
    pub no_tui: bool,

    #[arg(long, global = true)]
    pub json: bool,

    #[arg(long, global = true)]
    pub dry_run: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commands {
    Mine,
    Devices,
    Benchmark,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Repl,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCommand {
    Show,
    Validate,
}

pub fn parse() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_command_shape_parses() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--backend",
            "cuda",
            "--device",
            "0",
            "--intensity",
            "75",
            "--no-tui",
            "--dry-run",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(Commands::Mine)));
        assert_eq!(cli.backend, "cuda");
        assert_eq!(cli.device, Some(0));
        assert_eq!(cli.intensity, 75);
        assert!(cli.no_tui);
        assert!(cli.dry_run);

        let show = Cli::try_parse_from(["pickaxe", "config", "show"]).unwrap();
        assert!(matches!(
            show.command,
            Some(Commands::Config {
                command: ConfigCommand::Show
            })
        ));
    }

    #[test]
    fn clap_rejects_out_of_range_intensity() {
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--intensity", "9"]).is_err());
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--intensity", "101"]).is_err());
    }
}
