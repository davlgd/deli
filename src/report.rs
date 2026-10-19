//! Progress output. Every line goes to standard output (or, under the
//! wizard's progress screen, to a listener), to any extra sinks (the serial
//! console in the live session), and into a transcript written into the
//! installed system at the end.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::sync::mpsc::Sender;

/// What an installation reports, for a screen that shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Step {
        index: usize,
        total: usize,
        name: String,
    },
    Line(String),
    /// The disk is about to be written: from here on, a failed
    /// installation leaves it changed.
    Erasing,
}

#[derive(Default)]
pub struct Reporter {
    sinks: Vec<File>,
    transcript: Vec<String>,
    /// Who hears the progress instead of standard output.
    listener: Option<Sender<Progress>>,
}

impl Reporter {
    /// Send progress to `listener` rather than standard output, which a
    /// full-screen interface owns.
    pub fn listen(&mut self, listener: Sender<Progress>) {
        self.listener = Some(listener);
    }

    /// Back to standard output.
    pub fn unlisten(&mut self) {
        self.listener = None;
    }

    /// Also write every line to `path`: a console device, or a log file
    /// created (with its directory) readable by root only.
    pub fn tee(&mut self, path: &str) {
        let is_device = path.starts_with("/dev/");
        if !is_device
            && let Some(parent) = std::path::Path::new(path).parent()
            && let Err(e) = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
        {
            eprintln!("deli: cannot create {}: {e}", parent.display());
        }
        match OpenOptions::new()
            .append(true)
            .create(!is_device)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => self.sinks.push(file),
            Err(e) => eprintln!("deli: cannot write to {path}: {e}"),
        }
    }

    pub fn step(&mut self, index: usize, total: usize, name: &str) {
        self.emit(
            &format!("[{index}/{total}] {name}"),
            Progress::Step {
                index,
                total,
                name: name.to_owned(),
            },
        );
    }

    /// Say the disk is about to be erased, right before it is.
    pub fn erasing(&mut self, disk: &str) {
        self.emit(&format!("      erasing {disk}"), Progress::Erasing);
    }

    pub fn info(&mut self, message: &str) {
        self.line(&format!("      {message}"));
    }

    pub fn warn(&mut self, message: &str) {
        self.line(&format!("    ! {message}"));
    }

    /// Into the sinks and the transcript only: for what the terminal shows
    /// otherwise.
    pub fn record(&mut self, line: &str) {
        for sink in &mut self.sinks {
            let _ = writeln!(sink, "{line}\r");
        }
        self.transcript.push(line.to_owned());
    }

    pub fn line(&mut self, line: &str) {
        self.emit(line, Progress::Line(line.to_owned()));
    }

    fn emit(&mut self, line: &str, progress: Progress) {
        match &self.listener {
            // A listener that went away must not stop an installation.
            Some(listener) => {
                let _ = listener.send(progress);
            }
            // A terminal that goes away (a lost SSH session) must not stop
            // an installation: println! would panic, and panics abort.
            None => {
                let _ = writeln!(std::io::stdout(), "{line}");
            }
        }
        self.record(line);
    }

    pub fn transcript(&self) -> String {
        let mut text = self.transcript.join("\n");
        text.push('\n');
        text
    }
}
