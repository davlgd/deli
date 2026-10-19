//! Exherbo repositories and the world set: add the repositories asked for,
//! which clones them, and the packages to the world set, for the first
//! `cave resolve -x world` to build; then sync the repositories and build
//! paludis' metadata cache, if asked.

use std::path::Path;

use super::{Context, Step};
use crate::error::{Error, Result};
use crate::repositories;
use crate::system::{Command, TARGET};

pub struct Repositories;

impl Step for Repositories {
    fn name(&self) -> &'static str {
        "repositories and world"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let config = ctx.config;
        let wanted = &config.repositories;
        let real = ctx.sys.is_real();
        let enabled = if real {
            repositories::enabled(Path::new(TARGET))
        } else {
            Vec::new()
        };
        let added: Vec<&String> = wanted
            .enable
            .iter()
            .filter(|name| !enabled.contains(name))
            .collect();
        // cave writes each repository's configuration, and those it depends
        // on, then clones them. All in one resolution: each cave start loads
        // every repository already there, and one asked for may come along
        // with another, which cave would then refuse to add again.
        if !added.is_empty() {
            let names: Vec<&str> = added.iter().map(|name| name.as_str()).collect();
            ctx.info(format!(
                "adding {}, and the repositories they need",
                names.join(", ")
            ));
            ctx.sys.run(
                &Command::new("cave")
                    .args(["resolve", "--execute"])
                    .args(names.iter().map(|name| format!("repository/{name}")))
                    .in_target_online(),
            )?;
        }
        // cave exits 0 without doing anything when it needs a permission
        // it was not given.
        let enabled = if real {
            repositories::enabled(Path::new(TARGET))
        } else {
            wanted.enable.clone()
        };
        if let Some(missing) = added.iter().find(|name| !enabled.contains(name)) {
            return Err(Error::Other(format!(
                "cave did not add the {missing} repository"
            )));
        }
        let world = ctx.state.world.clone();
        if !world.is_empty() {
            ctx.sys.run(
                &Command::new("cave")
                    .arg("update-world")
                    .args(&world)
                    .in_target(),
            )?;
            ctx.info(format!(
                "in world, for the first cave resolve -x world to build: {}",
                world.join(" ")
            ));
        }
        if !wanted.sync {
            return Ok(());
        }
        // The system is installed and boots: a network failure here only
        // leaves the repositories as they were cloned.
        let failed = |ctx: &mut Context<'_>, what: &str, e: Error| {
            ctx.report.warn(&format!(
                "{what} failed, run it again on the installed system: {e}"
            ));
        };
        ctx.info("syncing every repository");
        if let Err(e) = ctx
            .sys
            .run(&Command::new("cave").arg("sync").in_target_online())
        {
            failed(ctx, "cave sync", e);
            return Ok(());
        }
        // A repository at a time: metadata takes minutes, the journal tells
        // how far it is.
        for (n, name) in enabled.iter().enumerate() {
            ctx.info(format!("metadata of {name} ({}/{})", n + 1, enabled.len()));
            let generate = Command::new("cave")
                .args(["generate-metadata", "--matching", &format!("*/*::{name}")])
                .in_target();
            if let Err(e) = ctx.sys.run(&generate) {
                failed(ctx, "cave generate-metadata", e);
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn repositories_are_added_then_synced() {
        let config = config("[repositories]\nenable = [\"net\", \"perl\"]\nsync = true\n");
        let actions = actions(&config, &facts(), |ctx| {
            // As preflight resolved it.
            ctx.state.world = vec!["sys-process/htop".into()];
            Repositories.run(ctx)
        });
        let at = |needle: &str| {
            actions
                .iter()
                .position(|a| a.contains(needle))
                .unwrap_or_else(|| panic!("no {needle} in {actions:?}"))
        };
        // One resolution for them all.
        assert!(
            at("-- cave resolve --execute repository/net repository/perl")
                < at("-- cave update-world sys-process/htop")
        );
        assert!(at("-- cave sync") < at("-- cave generate-metadata --matching */*::net"));
        assert!(at("*/*::net") < at("*/*::perl"));
        // Added to world, not built.
        assert!(!contains(&actions, "resolve world"), "{actions:?}");
    }

    #[test]
    fn nothing_asked_nothing_done() {
        let actions = actions(&config(""), &facts(), |ctx| Repositories.run(ctx));
        assert!(actions.is_empty(), "{actions:?}");
    }

    #[test]
    fn a_failed_sync_leaves_an_installed_system() {
        let config = config("[repositories]\nsync = true\n");
        let sys = crate::system::DrySys::failing_on("cave sync");
        with_context(&config, &facts(), &sys, |ctx| {
            Repositories.run(ctx).unwrap();
        });
        assert!(!contains(&sys.actions(), "generate-metadata"));
    }
}
