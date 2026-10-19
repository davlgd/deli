//! From a configuration to an installed system: resolve, show, confirm, run.

use std::io::{BufRead as _, Write as _};

use crate::config::Config;
use crate::error::{Error, ExitStatus, Result};
use crate::report::Reporter;
use crate::steps::{self, Context, State};
use crate::storage::{Plan, human};
use crate::system::probe::{Disk, Facts, resolve};
use crate::system::{Command, DrySys, RealSys, Sys, TARGET};

/// dracut's dmsquash-live mounts the squashfs here, under the overlay.
pub const LIVE_ROOT: &str = "/run/rootfsbase";

pub struct Options {
    pub dry_run: bool,
    /// Skip the confirmation prompt.
    pub yes: bool,
    pub source: String,
    /// A dry run prints nothing: progress goes to the reporter's listener.
    pub quiet: bool,
}

/// Held while an installation runs: the console's installer and one
/// started over SSH must not both write to a disk.
const LOCK: &str = "/run/deli/install.lock";

/// Left, under the lock, by a reboot under way: `systemctl reboot` returns
/// before the machine stops, and nothing may start installing meanwhile.
pub const REBOOTING: &str = "/run/deli/rebooting";

/// The lock of the only installation running, until it is dropped: also
/// taken to change the live session's network or to reboot it.
pub fn lock() -> Result<std::fs::File> {
    std::fs::create_dir_all("/run/deli").map_err(|e| Error::io("cannot create /run/deli", e))?;
    let file =
        std::fs::File::create(LOCK).map_err(|e| Error::io(format!("cannot create {LOCK}"), e))?;
    match file.try_lock() {
        Ok(()) if std::path::Path::new(REBOOTING).exists() => {
            Err(Error::Other("this machine is rebooting".into()))
        }
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::Other(
            "another installation is running on this machine, from the console or over SSH".into(),
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(Error::io(format!("cannot lock {LOCK}"), e)),
    }
}

/// Commands' output during an installation.
pub const COMMAND_LOG: &str = "/run/deli/commands.log";

/// Facts about this machine; in a dry run on a machine without them (a
/// developer's laptop), a stand-in for the configured disk.
pub fn facts_for(config: &Config, dry_run: bool) -> Result<Facts> {
    match Facts::gather() {
        Ok(facts) => Ok(facts),
        Err(e) if dry_run => {
            eprintln!("deli: cannot inspect this machine ({e}); pretending a 1T UEFI disk");
            let path = if config.storage.disk == "auto" {
                "/dev/sda".to_owned()
            } else {
                config.storage.disk.clone()
            };
            Ok(Facts {
                uefi: true,
                disks: vec![Disk {
                    path,
                    size: 1 << 40,
                    model: "stand-in".into(),
                    serial: String::new(),
                    removable: false,
                    read_only: false,
                    ..Default::default()
                }],
                ..Facts::default()
            })
        }
        Err(e) => Err(e),
    }
}

/// Where an installed system records which configuration produced it.
pub const FINGERPRINT: &str = "/etc/deli/config.sha256";

/// SHA-256 of the whole configuration, secrets included (as a hash, they do
/// not leak). Equal fingerprints mean "this very installation again".
pub fn fingerprint(sys: &dyn Sys, config: &Config) -> Result<String> {
    let full = toml::to_string(config)
        .map_err(|e| Error::Other(format!("cannot serialize the configuration: {e}")))?;
    let out = sys.capture(&Command::new("sha256sum").stdin(full))?;
    Ok(out.split_whitespace().next().unwrap_or_default().to_owned())
}

/// Validate and turn the configuration into a plan for this machine.
pub fn plan(config: &Config, facts: &Facts) -> Result<Plan> {
    config.validate()?;
    let disk = if config.storage.disk == "auto" {
        // Never guess between disks: one of them may hold data.
        match facts.candidate_disks().as_slice() {
            [disk] => disk.path.clone(),
            [] => {
                return Err(Error::Other(
                    "storage.disk = \"auto\": no fixed disk found".into(),
                ));
            }
            several => {
                let list: Vec<String> = several.iter().map(|d| d.describe()).collect();
                return Err(Error::Other(format!(
                    "storage.disk = \"auto\" needs exactly one candidate disk, found {}: {}; name the disk instead",
                    several.len(),
                    list.join(", ")
                )));
            }
        }
    } else {
        // Kernel name (/dev/sda), whatever alias the file used: mounts and
        // swaps are reported under it.
        facts
            .disk(&config.storage.disk)
            .map_or_else(|| resolve(&config.storage.disk), |d| d.path.clone())
    };
    Ok(Plan::new(&config.storage, &disk))
}

/// A failed attempt may have left its target mounted, on another disk than
/// this attempt's: let go of it before anything, so that neither the steps
/// nor the failure record write into it.
pub fn release_target(sys: &dyn Sys) -> Result<()> {
    let found = sys.capture(&Command::new("findmnt").args([
        "--noheadings",
        "--output",
        "TARGET",
        "--mountpoint",
        TARGET,
    ]));
    let mounted = match found {
        Ok(out) => !out.trim().is_empty(),
        // findmnt's "nothing mounted there"; anything else is a real failure.
        Err(Error::Command {
            status: ExitStatus::Code(1),
            ..
        }) => false,
        Err(e) => return Err(e),
    };
    if mounted {
        sys.run(&Command::new("umount").args(["--recursive", TARGET]))?;
    }
    Ok(())
}

fn confirm(plan: &Plan) -> Result<()> {
    print!(
        "\nEverything on {} will be erased. Type \"yes\" to continue: ",
        plan.disk
    );
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| Error::io("cannot read the answer", e))?;
    if answer.trim() == "yes" {
        Ok(())
    } else {
        Err(Error::Other("cancelled, nothing was changed".into()))
    }
}

pub fn run(config: &Config, facts: &Facts, options: &Options, report: &mut Reporter) -> Result<()> {
    let plan = plan(config, facts)?;
    let size = facts
        .disk(&plan.disk)
        .map(|d| human(d.size))
        .unwrap_or_default();
    report.line(&format!(
        "deli {}: installing {} on {} ({size})",
        env!("CARGO_PKG_VERSION"),
        config.system.hostname,
        plan.disk
    ));
    for line in plan.explain().lines() {
        report.line(&format!("  {line}"));
    }
    if !options.dry_run {
        report.line(&format!("  commands' output: {COMMAND_LOG}"));
    }
    if !options.dry_run && !options.yes {
        confirm(&plan)?;
    }
    let _lock = if options.dry_run { None } else { Some(lock()?) };
    // The machine may have changed while the question waited: erase only
    // the disk that was planned for.
    let gathered;
    let facts = if options.dry_run {
        facts
    } else {
        gathered = Facts::gather()?;
        if gathered.disk(&plan.disk) != facts.disk(&plan.disk) {
            return Err(Error::Other(format!(
                "{} changed since the installation was planned (a disk was added, removed or replaced): nothing was erased",
                plan.disk
            )));
        }
        &gathered
    };

    // Commands write to their log, never to a terminal that may go away
    // (a lost SSH session) in the middle of an installation.
    let real = RealSys::logging_to(COMMAND_LOG);
    let dry = if options.quiet {
        DrySys::default()
    } else {
        DrySys::echoing()
    };
    let sys: &dyn Sys = if options.dry_run { &dry } else { &real };
    release_target(sys)?;
    let mut ctx = Context {
        config,
        plan: &plan,
        facts,
        sys,
        report,
        source: &options.source,
        state: State::default(),
    };
    if let Err(error) = steps::run(&mut ctx) {
        // The caller shows the error; the log and the transcript keep it.
        ctx.report.record(&format!("FAILED: {error}"));
        // Keep what happened on the disk if the target got far enough.
        if sys.is_real() && std::path::Path::new(TARGET).join("etc/os-release").exists() {
            let _ = steps::record_failure(&mut ctx);
        }
        return Err(error);
    }
    ctx.report.line("done: the system is installed");
    Ok(())
}
