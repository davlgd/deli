//! Effects that are only recorded: `--dry-run`, and tests.

use std::cell::RefCell;

use super::{Command, Sys};
use crate::error::Result;

#[derive(Debug, Default)]
pub struct DrySys {
    actions: RefCell<Vec<String>>,
    echo: bool,
    /// Tests: commands containing this fail, as on a real machine.
    #[cfg(test)]
    fail_on: Option<&'static str>,
    /// Tests: what a captured command containing the first prints.
    #[cfg(test)]
    outputs: Vec<(&'static str, &'static str)>,
}

impl DrySys {
    /// A recorder that also prints each action as it is recorded.
    pub fn echoing() -> Self {
        Self {
            echo: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub fn failing_on(needle: &'static str) -> Self {
        Self {
            fail_on: Some(needle),
            ..Self::default()
        }
    }

    /// Tests: captured commands containing `needle` print `output`.
    #[cfg(test)]
    pub fn answering(mut self, needle: &'static str, output: &'static str) -> Self {
        self.outputs.push((needle, output));
        self
    }

    #[cfg(test)]
    pub fn actions(&self) -> Vec<String> {
        self.actions.borrow().clone()
    }

    /// Every command succeeds, except in tests those `fail_on` names.
    #[cfg_attr(
        not(test),
        allow(clippy::unused_self, clippy::unnecessary_wraps, unused_variables)
    )]
    fn outcome(&self, command: &Command) -> Result<()> {
        #[cfg(test)]
        if self
            .fail_on
            .is_some_and(|n| command.to_string().contains(n))
        {
            return Err(crate::error::Error::Other(format!(
                "simulated failure of `{command}`"
            )));
        }
        Ok(())
    }

    fn record(&self, line: String) {
        if self.echo {
            println!("    {line}");
        }
        self.actions.borrow_mut().push(line);
    }
}

impl Sys for DrySys {
    fn run(&self, command: &Command) -> Result<()> {
        // stdin is deliberately not shown: it carries secrets.
        self.record(format!("run: {command}"));
        self.outcome(command)
    }

    fn capture(&self, command: &Command) -> Result<String> {
        self.record(format!("capture: {command}"));
        self.outcome(command)?;
        #[cfg(test)]
        if let Some((_, output)) = self
            .outputs
            .iter()
            .find(|(needle, _)| command.to_string().contains(needle))
        {
            return Ok((*output).to_owned());
        }
        Ok(String::new())
    }

    fn write(&self, path: &str, contents: &str, mode: u32) -> Result<()> {
        self.record(format!("write {path} ({mode:o}):"));
        for line in contents.lines() {
            self.record(format!("  | {line}"));
        }
        Ok(())
    }

    fn create_sparse(&self, path: &str, size: u64) -> Result<()> {
        self.record(format!("create {path} ({size} bytes, sparse)"));
        Ok(())
    }

    fn mkdir_p(&self, path: &str, mode: u32) -> Result<()> {
        self.record(format!("mkdir {path} ({mode:o})"));
        Ok(())
    }

    fn remove(&self, path: &str) -> Result<()> {
        self.record(format!("remove {path}"));
        Ok(())
    }

    fn symlink(&self, to: &str, link: &str) -> Result<()> {
        self.record(format!("symlink {link} -> {to}"));
        Ok(())
    }

    fn is_real(&self) -> bool {
        false
    }
}
