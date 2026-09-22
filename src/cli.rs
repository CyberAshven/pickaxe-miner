//! Single Clap startup parser for Pickaxe.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "pickaxe", about = "Pickaxe Miner - GPU-only PHOTON miner")]
pub struct Cli {
    #[arg(long, global = true, default_value = "auto", value_parser = ["auto", "cuda", "hip"])]
    pub backend: String,

    #[arg(long, global = true)]
    pub device: Option<u32>,

    #[arg(
        long,
        global = true,
        value_parser = clap::value_parser!(u8).range(10..=100),
        help = "GPU intensity 10..=100 (mine defaults to 100; benchmark without this flag runs 10/25/50/75/100)"
    )]
    pub intensity: Option<u8>,

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

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commands {
    Mine,
    Devices,
    SelfTest,
    Benchmark {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        #[arg(long)]
        ui_compare: bool,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    #[command(hide = true)]
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
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(Commands::Mine)));
        assert_eq!(cli.backend, "cuda");
        assert_eq!(cli.device, Some(0));
        assert_eq!(cli.intensity, Some(75));
        assert!(cli.no_tui);

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

    #[test]
    fn clap_rejects_detached_wgpu_backend() {
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--backend", "wgpu"]).is_err());
    }

    #[test]
    fn clap_parses_offline_self_test() {
        let cli = Cli::try_parse_from(["pickaxe", "self-test", "--backend", "cuda"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::SelfTest)));
        assert_eq!(cli.backend, "cuda");
    }

    #[test]
    fn clap_parses_offline_benchmark_window() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "benchmark",
            "--backend",
            "cuda",
            "--seconds",
            "7",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Benchmark {
                seconds: 7,
                ui_compare: false
            })
        ));
        assert_eq!(cli.backend, "cuda");
        assert_eq!(cli.intensity, None);

        let selected = Cli::try_parse_from([
            "pickaxe",
            "benchmark",
            "--backend",
            "cuda",
            "--seconds",
            "3",
            "--intensity",
            "30",
        ])
        .unwrap();
        assert_eq!(selected.intensity, Some(30));

        let ui = Cli::try_parse_from([
            "pickaxe",
            "benchmark",
            "--backend",
            "cuda",
            "--seconds",
            "3",
            "--ui-compare",
        ])
        .unwrap();
        assert!(matches!(
            ui.command,
            Some(Commands::Benchmark {
                seconds: 3,
                ui_compare: true
            })
        ));
    }

    #[test]
    fn clap_rejects_removed_live_dry_run_flag() {
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--dry-run"]).is_err());
    }
}
