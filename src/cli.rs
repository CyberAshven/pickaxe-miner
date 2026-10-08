//! Single Clap startup parser for Pickaxe.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "pickaxe", version, about = "Pickaxe Miner - CashToken mining")]
pub struct Cli {
    #[arg(long, global = true, value_parser = ["mainnet", "chipnet"], conflicts_with = "chipnet")]
    pub network: Option<String>,

    /// Shorthand for --network chipnet.
    #[arg(long, global = true)]
    pub chipnet: bool,

    /// Token name, category ID, or covenant locking bytecode.
    #[arg(long, global = true)]
    pub token: Option<String>,

    #[arg(long, global = true, value_parser = ["auto", "cuda", "hip", "wgpu"])]
    pub backend: Option<String>,

    /// GPUs to mine on: all, one number from `pickaxe devices`, or a list like 0,2.
    /// By default every discrete GPU mines (integrated GPUs only when there is
    /// no discrete GPU).
    #[arg(long, global = true, value_name = "all|N|N,M", value_parser = crate::backend::DeviceSelection::parse)]
    pub device: Option<crate::backend::DeviceSelection>,

    /// Mine on integrated GPUs too when --device is all (the default).
    #[arg(long, global = true)]
    pub include_integrated: bool,

    #[arg(
        long,
        global = true,
        value_parser = clap::value_parser!(u8).range(10..=100),
        help = "GPU intensity 10..=100 (setup restores a saved profile; otherwise 100; benchmark without this flag runs 10/25/50/75/100)"
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

    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

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
    /// BCH Stratum V2 (ASIC-facing; see docs/stratum-v2.md).
    StratumV2 {
        #[command(subcommand)]
        command: StratumV2Command,
    },
    #[command(hide = true)]
    Repl,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCommand {
    Show,
    Validate,
    Save,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StratumV2Command {
    /// Print implementation and validation status.
    Status,
    /// Check the configured BCH node and full block template without mining.
    CheckNode,
    /// Show a running server's workers table, read-only (for example a
    /// service started with --no-tui). Use the same --config as the server.
    Watch,
    /// Serve encrypted BCH mining jobs to SV2 devices.
    Serve {
        /// Listener address. Use a LAN address to connect an external ASIC.
        #[arg(long, default_value = "127.0.0.1:3336")]
        listen: std::net::SocketAddr,
        /// Optional plain SV1 endpoint for ASIC firmware on a trusted LAN.
        #[arg(long)]
        sv1_listen: Option<std::net::SocketAddr>,
        /// BCH donation percentage, 0 to 100 (default 1.5). Defaults to the saved setting.
        #[arg(long)]
        donation: Option<crate::donation::bch::BchDonation>,
    },
}

/// Parses command-line arguments into the supported miner commands.
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
        assert_eq!(cli.backend.as_deref(), Some("cuda"));
        assert_eq!(
            cli.device,
            Some(crate::backend::DeviceSelection::Indices(vec![0]))
        );
        assert_eq!(cli.intensity, Some(75));
        assert!(cli.no_tui);

        let show = Cli::try_parse_from(["pickaxe", "config", "show"]).unwrap();
        assert!(matches!(
            show.command,
            Some(Commands::Config {
                command: ConfigCommand::Show
            })
        ));

        let save =
            Cli::try_parse_from(["pickaxe", "config", "save", "--config", "pickaxe.json"]).unwrap();
        assert!(matches!(
            save.command,
            Some(Commands::Config {
                command: ConfigCommand::Save
            })
        ));
        assert_eq!(save.config, Some(PathBuf::from("pickaxe.json")));
    }

    #[test]
    fn clap_rejects_out_of_range_intensity() {
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--intensity", "9"]).is_err());
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--intensity", "101"]).is_err());
    }

    #[test]
    fn device_flag_takes_all_a_number_or_a_list() {
        use crate::backend::DeviceSelection;
        let parse = |args: &[&str]| Cli::try_parse_from(args).map(|cli| cli.device);
        assert_eq!(
            parse(&["pickaxe", "mine", "--device", "all"]).unwrap(),
            Some(DeviceSelection::Default)
        );
        assert_eq!(
            parse(&["pickaxe", "mine", "--device", "0,2"]).unwrap(),
            Some(DeviceSelection::Indices(vec![0, 2]))
        );
        assert!(parse(&["pickaxe", "mine", "--device", "gpu0"]).is_err());
        assert!(parse(&["pickaxe", "mine", "--device", "1,1"]).is_err());
        let cli = Cli::try_parse_from(["pickaxe", "mine", "--include-integrated"]).unwrap();
        assert!(cli.include_integrated);
        assert_eq!(cli.device, None);
    }

    #[test]
    fn clap_accepts_wgpu_backend_surface() {
        let cli = Cli::try_parse_from(["pickaxe", "devices", "--backend", "wgpu"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Devices)));
        assert_eq!(cli.backend.as_deref(), Some("wgpu"));
    }

    #[test]
    fn clap_parses_offline_self_test() {
        let cli = Cli::try_parse_from(["pickaxe", "self-test", "--backend", "cuda"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::SelfTest)));
        assert_eq!(cli.backend.as_deref(), Some("cuda"));
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
        assert_eq!(cli.backend.as_deref(), Some("cuda"));
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

    #[test]
    fn mining_network_and_token_flags_parse_after_command() {
        let cli =
            Cli::try_parse_from(["pickaxe", "mine", "--chipnet", "--token", "PHOTON"]).unwrap();
        assert!(cli.chipnet);
        assert_eq!(cli.token.as_deref(), Some("PHOTON"));
        assert!(
            Cli::try_parse_from(["pickaxe", "mine", "--chipnet", "--network", "mainnet"]).is_err()
        );
    }

    #[test]
    fn clap_parses_stratum_v2_status() {
        let cli = Cli::try_parse_from(["pickaxe", "stratum-v2", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::StratumV2 {
                command: StratumV2Command::Status
            })
        ));
    }

    #[test]
    fn clap_parses_stratum_v2_watch_without_server_options() {
        let cli = Cli::try_parse_from(["pickaxe", "--chipnet", "stratum-v2", "watch"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::StratumV2 {
                command: StratumV2Command::Watch
            })
        ));
    }

    #[test]
    fn bch_donation_flag_accepts_percentages_from_zero_to_one_hundred() {
        for (text, expected) in [("0", 0), ("1.5", 150), ("2.01", 201), ("100", 10_000)] {
            let cli = Cli::try_parse_from(["pickaxe", "stratum-v2", "serve", "--donation", text])
                .unwrap();
            let Some(Commands::StratumV2 {
                command:
                    StratumV2Command::Serve {
                        donation: Some(rate),
                        ..
                    },
            }) = cli.command
            else {
                panic!("donation missing")
            };
            assert_eq!(u16::from(rate), expected);
        }
        for text in ["-1", "100.01", "2.001", "NaN"] {
            assert!(
                Cli::try_parse_from(["pickaxe", "stratum-v2", "serve", "--donation", text])
                    .is_err()
            );
        }
    }

    #[test]
    fn stratum_server_requires_a_valid_listener_and_preserves_global_network() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--chipnet",
            "--listen",
            "127.0.0.1:3336",
        ])
        .unwrap();
        assert!(cli.chipnet);
        assert!(matches!(
            cli.command,
            Some(Commands::StratumV2 {
                command: StratumV2Command::Serve { .. }
            })
        ));
        assert!(Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--listen",
            "not-a-listener"
        ])
        .is_err());
        assert!(Cli::try_parse_from(["pickaxe", "stratum-v2", "check-node", "--chipnet"]).is_ok());
    }
}
