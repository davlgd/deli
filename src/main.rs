mod config;
mod error;
mod image;
mod install;
mod launch;
mod network;
mod report;
mod repositories;
mod sshkey;
mod steps;
mod storage;
mod system;
mod wizard;

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::error::Result;
use crate::install::LIVE_ROOT;
use crate::launch::Request;

/// Exherbo Linux server installer.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install from a configuration file, or ask with the interactive
    /// installer when there is none: `--config`, `deli.toml` here, then, in
    /// the live session, `deli.autoinstall=<url>` or a CIDATA volume.
    Install {
        #[arg(long, short)]
        config: Option<PathBuf>,
        /// Print every action instead of performing it.
        #[arg(long)]
        dry_run: bool,
        /// Do not ask before erasing the disk.
        #[arg(long, short)]
        yes: bool,
        /// Stay in the live session when done.
        #[arg(long)]
        no_reboot: bool,
        /// Root filesystem to copy, for a dry run: an installation copies
        /// the live session's.
        #[arg(long, default_value = LIVE_ROOT)]
        source: String,
    },
    /// Build a raw disk image of a root filesystem, configured by
    /// cloud-init on its first boot: for virtual machines and clouds. Needs
    /// root and loop devices.
    Image {
        /// Root filesystem to put in the image.
        #[arg(long)]
        source: String,
        /// Image file to create.
        output: String,
    },
    /// Validate a configuration and show the storage plan for this machine.
    Plan {
        #[arg(long, short)]
        config: PathBuf,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Best effort: the terminal may be gone.
            let _ = writeln!(std::io::stderr(), "deli: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Install {
            config,
            dry_run,
            yes,
            no_reboot,
            source,
        } => launch::install(&Request {
            config,
            dry_run,
            yes,
            no_reboot,
            source,
        }),
        Commands::Image { source, output } => {
            // The image is mounted where an installation mounts its target.
            let _lock = install::lock()?;
            image::build(
                &crate::system::RealSys::default(),
                &source,
                &output,
                &mut crate::report::Reporter::default(),
            )
        }
        Commands::Plan { config } => {
            let config = Config::load(&config)?;
            let facts = install::facts_for(&config, true)?;
            let plan = install::plan(&config, &facts)?;
            print!("{}", plan.explain());
            Ok(())
        }
    }
}
