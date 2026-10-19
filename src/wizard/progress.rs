//! The installation running under the wizard: what it reports while it runs,
//! then how it ended and what the user does next. Nothing here touches the
//! terminal.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::KeyCode;

use crate::report::Progress;

/// Entries of the journal kept for the screen; the full journal is on disk.
const KEPT: usize = 200;

/// A line of the journal: a step that started, or what one reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Step { index: usize, name: String },
    Line(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Running,
    Done,
    Failed(String),
}

/// What the user chose once the installation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Reboot,
    Shell,
    BackToInstaller,
}

pub struct Installation {
    /// The disk being installed, as the user confirmed it.
    pub target: String,
    /// What the done screen tells about the new system.
    pub facts: Vec<(&'static str, String)>,
    pub phase: Phase,
    /// `(index, total, name)` of the running step.
    pub step: Option<(usize, usize, String)>,
    pub journal: Vec<Entry>,
    /// The disk started being erased.
    pub erasing: bool,
    started: Instant,
    ended: Option<Duration>,
    /// Focused button of the end screen.
    pub button: usize,
    /// First line shown of the failure's text.
    pub scroll: usize,
}

impl Installation {
    pub fn new(target: String, facts: Vec<(&'static str, String)>) -> Self {
        Self {
            target,
            facts,
            phase: Phase::Running,
            step: None,
            journal: Vec::new(),
            erasing: false,
            started: Instant::now(),
            ended: None,
            button: 1,
            scroll: 0,
        }
    }

    pub fn report(&mut self, progress: Progress) {
        match progress {
            Progress::Step { index, total, name } => {
                self.journal.push(Entry::Step {
                    index,
                    name: name.clone(),
                });
                self.step = Some((index, total, name));
            }
            Progress::Erasing => self.erasing = true,
            Progress::Line(line) => self.journal.push(Entry::Line(line.trim().to_owned())),
        }
        if self.journal.len() > KEPT {
            self.journal.remove(0);
        }
    }

    pub fn extend(&mut self, progress: impl IntoIterator<Item = Progress>) {
        for p in progress {
            self.report(p);
        }
    }

    pub fn finish(&mut self, result: Result<(), String>) {
        self.ended = Some(self.started.elapsed());
        self.phase = match result {
            Ok(()) => Phase::Done,
            Err(error) => Phase::Failed(error),
        };
    }

    pub fn elapsed(&self) -> Duration {
        self.ended.unwrap_or_else(|| self.started.elapsed())
    }

    /// The end screen's buttons, main one last.
    pub fn buttons(&self) -> &'static [(Choice, &'static str)] {
        match self.phase {
            Phase::Running => &[],
            Phase::Done => &[(Choice::Shell, "Live shell"), (Choice::Reboot, "Reboot")],
            Phase::Failed(_) => &[
                (Choice::Shell, "Live shell"),
                (Choice::BackToInstaller, "Back to the installer"),
            ],
        }
    }

    /// A key on the end screen; nothing stops a running installation.
    pub fn handle(&mut self, code: KeyCode) -> Option<Choice> {
        let count = self.buttons().len();
        if count == 0 {
            return None;
        }
        match code {
            KeyCode::Left | KeyCode::BackTab | KeyCode::Up => {
                self.button = self.button.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Tab | KeyCode::Down => {
                self.button = (self.button + 1).min(count - 1);
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
            KeyCode::PageDown => self.scroll += 5,
            KeyCode::Enter | KeyCode::Char(' ') => {
                return self.buttons().get(self.button).map(|(choice, _)| *choice);
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(index: usize) -> Progress {
        Progress::Step {
            index,
            total: 13,
            name: "a step".into(),
        }
    }

    #[test]
    fn running_ignores_keys_then_offers_reboot() {
        let mut run = Installation::new("/dev/sda".into(), Vec::new());
        run.report(step(1));
        assert!(!run.erasing);
        assert_eq!(run.handle(KeyCode::Enter), None);
        run.report(step(2));
        run.report(Progress::Erasing);
        assert!(run.erasing);
        run.finish(Ok(()));
        // Reboot is the main button; Left reaches the shell.
        assert_eq!(run.handle(KeyCode::Enter), Some(Choice::Reboot));
        run.handle(KeyCode::Left);
        assert_eq!(run.handle(KeyCode::Enter), Some(Choice::Shell));
    }

    #[test]
    fn a_failure_goes_back_to_the_installer() {
        let mut run = Installation::new("/dev/sda".into(), Vec::new());
        for i in 0..300 {
            run.report(Progress::Line(format!("line {i}")));
        }
        assert_eq!(run.journal.len(), KEPT);
        run.finish(Err("boom".into()));
        assert_eq!(run.phase, Phase::Failed("boom".into()));
        assert_eq!(run.handle(KeyCode::Enter), Some(Choice::BackToInstaller));
    }
}
