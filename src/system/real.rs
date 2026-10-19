//! Effects that really happen.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink};
use std::path::Path;
use std::process::Stdio;

use super::{Command, Sys};
use crate::error::{Error, ExitStatus, Result};

#[derive(Debug, Default)]
pub struct RealSys {
    /// Where commands' output goes instead of the terminal, which a
    /// full-screen interface owns.
    output_log: Option<&'static str>,
}

impl RealSys {
    pub fn logging_to(path: &'static str) -> Self {
        Self {
            output_log: Some(path),
        }
    }

    /// The output log, opened for `command` and its header written; where
    /// the command's own output will start in it.
    fn open_log(&self, command: &Command) -> Result<Option<(fs::File, u64)>> {
        let Some(path) = self.output_log else {
            return Ok(None);
        };
        let error = |e| Error::io(format!("cannot write {path}"), e);
        if let Some(parent) = Path::new(path).parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(error)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(error)?;
        writeln!(file, "$ {command}").map_err(error)?;
        let start = file.metadata().map_err(error)?.len();
        Ok(Some((file, start)))
    }

    fn execute(&self, command: &Command, capture: bool) -> Result<String> {
        // Under a full-screen interface, a command that only acts writes its
        // output straight into the log, which can be read while it runs. A
        // captured output (it may be a password's hash) never goes there.
        let log = if capture {
            None
        } else {
            self.open_log(command)?
        };
        let to_log = |file: &fs::File| {
            file.try_clone()
                .map(Stdio::from)
                .map_err(|e| Error::io("cannot share the command log", e))
        };
        let (stdout, stderr) = match &log {
            Some((file, _)) => (to_log(file)?, to_log(file)?),
            None if capture => (Stdio::piped(), Stdio::piped()),
            None => (Stdio::inherit(), Stdio::piped()),
        };
        let mut child = std::process::Command::new(&command.program);
        child
            .args(&command.args)
            .envs(command.env.iter().map(|(k, v)| (k, v)))
            .stdin(if command.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(stdout)
            .stderr(stderr);
        let mut child = child
            .spawn()
            .map_err(|e| Error::io(format!("cannot start `{}`", command.program), e))?;
        if let Some(input) = &command.stdin {
            // Inputs are small (passphrases, short files): written in full
            // before output is drained, which a pipe buffer absorbs.
            let mut stdin = child.stdin.take().expect("stdin was piped");
            stdin
                .write_all(input.as_bytes())
                .map_err(|e| Error::io(format!("cannot write to `{}`", command.program), e))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|e| Error::io(format!("cannot wait for `{}`", command.program), e))?;
        let stderr = || match (&log, self.output_log) {
            (Some((_, start)), Some(path)) => tail(path, *start),
            _ => String::from_utf8_lossy(&output.stderr).into_owned(),
        };
        if !output.status.success() {
            let stderr = stderr();
            return Err(Error::Command {
                command: command.to_string(),
                status: output
                    .status
                    .code()
                    .map_or(ExitStatus::Signal, ExitStatus::Code),
                stderr,
            });
        }
        // Successful commands may still warn; keep it visible.
        if !capture && log.is_none() {
            let stderr = stderr();
            // A terminal that went away must not stop deli: eprint! would
            // panic, and panics abort.
            if !stderr.trim().is_empty() {
                let _ = write!(std::io::stderr(), "{stderr}");
            }
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// The end of what a command wrote into the log from `start`: enough to say
/// why it failed, read without loading the whole log.
fn tail(path: &str, start: u64) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    const KEPT: u64 = 4096;
    let Ok(mut file) = fs::File::open(path) else {
        return String::new();
    };
    let end = file.metadata().map_or(start, |m| m.len());
    let mut text = Vec::new();
    if file
        .seek(SeekFrom::Start(start.max(end.saturating_sub(KEPT))))
        .is_ok()
    {
        let _ = file.take(KEPT).read_to_end(&mut text);
    }
    String::from_utf8_lossy(&text).into_owned()
}

impl Sys for RealSys {
    fn run(&self, command: &Command) -> Result<()> {
        self.execute(command, false).map(drop)
    }

    fn capture(&self, command: &Command) -> Result<String> {
        self.execute(command, true)
    }

    fn write(&self, path: &str, contents: &str, mode: u32) -> Result<()> {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("cannot create {}", parent.display()), e))?;
        }
        // The mode is set before the contents are written: a secret is never
        // readable by others, not even for a moment.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(path)
            .map_err(|e| Error::io(format!("cannot write {path}"), e))?;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|e| Error::io(format!("cannot chmod {path}"), e))?;
        file.write_all(contents.as_bytes())
            .map_err(|e| Error::io(format!("cannot write {path}"), e))
    }

    fn create_sparse(&self, path: &str, size: u64) -> Result<()> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(path)
            .and_then(|file| file.set_len(size))
            .map_err(|e| Error::io(format!("cannot create {path}"), e))
    }

    fn mkdir_p(&self, path: &str, mode: u32) -> Result<()> {
        fs::create_dir_all(path).map_err(|e| Error::io(format!("cannot create {path}"), e))?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| Error::io(format!("cannot chmod {path}"), e))
    }

    fn remove(&self, path: &str) -> Result<()> {
        let result = match fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => Err(e),
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
            Ok(_) => fs::remove_file(path),
        };
        result.map_err(|e| Error::io(format!("cannot remove {path}"), e))
    }

    fn symlink(&self, to: &str, link: &str) -> Result<()> {
        self.remove(link)?;
        if let Some(parent) = Path::new(link).parent() {
            fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("cannot create {}", parent.display()), e))?;
        }
        symlink(to, link).map_err(|e| Error::io(format!("cannot link {link} -> {to}"), e))
    }

    fn is_real(&self) -> bool {
        true
    }
}
