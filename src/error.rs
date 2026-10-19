//! Errors deli reports. Every failure ends up as one of these, with enough
//! context (the command, its stderr) to act on without rerunning verbosely.

use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration: {0}")]
    Config(String),

    #[error("invalid configuration:\n{}", .0.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n"))]
    Invalid(Vec<String>),

    #[error("`{command}` failed ({status}){}", stderr_suffix(.stderr))]
    Command {
        command: String,
        status: ExitStatus,
        stderr: String,
    },

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Other(String),
}

/// An exit code, or the signal that killed the process.
#[derive(Debug, Clone, Copy)]
pub enum ExitStatus {
    Code(i32),
    Signal,
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(code) => write!(f, "exit code {code}"),
            Self::Signal => f.write_str("killed by a signal"),
        }
    }
}

fn stderr_suffix(stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        String::new()
    } else {
        format!(":\n{stderr}")
    }
}

impl Error {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
