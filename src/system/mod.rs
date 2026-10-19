//! The effects boundary.
//!
//! Every side effect deli performs goes through [`Sys`]: running a command,
//! writing a file. [`RealSys`] acts; [`DrySys`] records and prints. Read-only
//! inspection of the machine lives in [`probe`], so a dry run still sees the
//! real disks.

mod dry;
mod real;

pub mod probe;
pub mod resolvers;

pub use dry::DrySys;
pub use real::RealSys;

use std::fmt;

use crate::error::Result;

/// Where the target system is mounted during installation.
pub const TARGET: &str = "/run/deli/target";

/// A path inside the target system, as seen from the live system.
pub fn target_path(path: &str) -> String {
    format!("{TARGET}/{}", path.trim_start_matches('/'))
}

/// Whether systemd runs this machine. In a container without it, where
/// images are built, systemd-nspawn has no bus to ask for a scope with, and
/// must stay in its caller's: which it refuses in a login session, where an
/// installation over SSH runs.
fn booted() -> bool {
    std::path::Path::new("/run/systemd/system").is_dir()
}

/// A program and its arguments. Run without a shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub env: Vec<(String, String)>,
}

impl Command {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            stdin: None,
            env: Vec::new(),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Data for the program's standard input: how secrets are passed, so
    /// they never appear in argv, logs or error messages.
    #[must_use]
    pub fn stdin(mut self, data: impl Into<String>) -> Self {
        self.stdin = Some(data.into());
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Run this command inside the target system, in a container that sees
    /// the target's own files (including its mounted ESP) but not the live
    /// system's. Used for tools that must act as the installed system would:
    /// kernel-install, dracut, localedef, chown by user name.
    #[must_use]
    pub fn in_target(self) -> Self {
        self.contained(&[])
    }

    /// [`Self::in_target`], resolving names too: the target's
    /// `/etc/resolv.conf` points into `/run/systemd/resolve`, which the
    /// container sees once the live session's is bound there. Its network is
    /// the live session's.
    #[must_use]
    pub fn in_target_online(self) -> Self {
        self.contained(&["--bind-ro=/run/systemd/resolve"])
    }

    fn contained(self, options: &[&str]) -> Self {
        let mut wrapped = Command::new("systemd-nspawn")
            .args([
                "--quiet",
                "--pipe",
                "--register=no",
                "--resolv-conf=off",
                "--timezone=off",
                "--link-journal=no",
            ])
            .args(options.iter().copied())
            .args((!booted()).then_some("--keep-unit"))
            .arg(format!("--directory={TARGET}"));
        for (key, value) in &self.env {
            wrapped = wrapped.arg(format!("--setenv={key}={value}"));
        }
        wrapped.args.push("--".to_owned());
        wrapped.args.push(self.program);
        wrapped.args.extend(self.args);
        wrapped.stdin = self.stdin;
        wrapped
    }
}

// Illustrative only (logs, dry runs, errors): arguments with spaces are
// quoted for readability, nothing is shell-escaped. Commands never go
// through a shell.
impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (key, value) in &self.env {
            write!(f, "{key}={value} ")?;
        }
        f.write_str(&self.program)?;
        for arg in &self.args {
            if arg.is_empty() || arg.chars().any(char::is_whitespace) {
                write!(f, " \"{arg}\"")?;
            } else {
                write!(f, " {arg}")?;
            }
        }
        Ok(())
    }
}

/// Every mutation deli performs, on the live system or on the target.
pub trait Sys {
    /// Run a command; error if it exits non-zero.
    fn run(&self, command: &Command) -> Result<()>;

    /// Run a command and return its standard output. In a dry run nothing
    /// runs and the output is empty: callers that branch on it check
    /// [`Self::is_real`].
    fn capture(&self, command: &Command) -> Result<String>;

    /// Create or replace a file, creating missing parent directories.
    fn write(&self, path: &str, contents: &str, mode: u32) -> Result<()>;

    /// Create a new file of `size` bytes, sparse; fail if anything,
    /// a dangling symbolic link included, is already there.
    fn create_sparse(&self, path: &str, size: u64) -> Result<()>;

    /// Create a directory and its missing parents.
    fn mkdir_p(&self, path: &str, mode: u32) -> Result<()>;

    /// Remove a file, symlink or directory tree. Missing paths are fine.
    fn remove(&self, path: &str) -> Result<()>;

    /// Create a symbolic link at `link` pointing to `to`, replacing `link`.
    fn symlink(&self, to: &str, link: &str) -> Result<()>;

    /// Whether effects really happen.
    fn is_real(&self) -> bool;
}
