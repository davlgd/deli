//! Refuse early, before anything is erased.

use std::path::Path;

use super::{Context, Step};
use crate::error::{Error, Result};
use crate::network;
use crate::repositories::Index;
use crate::storage::human;
use crate::system::Command;

/// The locale's sources and the keymap must exist in the system about to be
/// copied: localedef and systemd-firstboot run after the disk is erased.
fn available_locale_and_keymap(root: &str, locale: &str, keymap: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let root = Path::new(root);
    if !super::compiled_locale(locale) {
        let (name, charset) = locale.split_once('.').unwrap_or((locale, ""));
        let charmaps = root.join("usr/share/i18n/charmaps");
        if !root.join("usr/share/i18n/locales").join(name).is_file()
            || !(charmaps.join(format!("{charset}.gz")).is_file()
                || charmaps.join(charset).is_file())
        {
            problems.push(format!("system.locale: \"{locale}\" is not available"));
        }
    }
    if !keymap_exists(&root.join("usr/share/keymaps"), keymap) {
        problems.push(format!("system.keymap: \"{keymap}\" is not available"));
    }
    problems
}

fn keymap_exists(dir: &Path, keymap: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(std::result::Result::ok).any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            keymap_exists(&path, keymap)
        } else {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name == format!("{keymap}.map.gz") || name == format!("{keymap}.map")
        }
    })
}

/// Repositories must be known to Exherbo's indexes and packages to the
/// repositories, which resolves bare package names; adding or syncing
/// repositories needs Exherbo's git.
fn software_problems(ctx: &mut Context<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    let config = ctx.config;
    let wanted = &config.repositories;
    ctx.state.world.clone_from(&config.packages.world);
    if ctx.sys.is_real() {
        let index = Index::read(Path::new(ctx.source));
        for name in wanted.enable.iter().filter(|name| !index.knows(name)) {
            problems.push(format!(
                "repositories.enable: \"{name}\" is in none of Exherbo's repository indexes"
            ));
        }
        ctx.state.world = config
            .packages
            .world
            .iter()
            .filter_map(|spec| {
                index
                    .resolve(spec, &wanted.enable)
                    .map_err(|e| problems.push(format!("packages.world: {e}")))
                    .ok()
            })
            .collect();
    }
    if (wanted.sync || !wanted.enable.is_empty())
        && let Err(e) = network::reachable(ctx.sys, network::EXHERBO_GIT)
    {
        problems.push(format!(
            "repositories: {} cannot be reached to clone or sync them ({e}); without repositories.enable and sync, the installation needs no network",
            network::EXHERBO_GIT
        ));
    }
    problems
}

/// A real installation copies the live session's system, nothing else:
/// another source could hold the target, or lie on the disk about to be
/// erased (through a loop device or an overlay). It must have a kernel.
fn source_problems(source: &str) -> Vec<String> {
    let mut problems = Vec::new();
    if source != crate::install::LIVE_ROOT {
        problems.push(format!(
            "{source}: an installation copies the live session's system, {}; another source is for dry runs only",
            crate::install::LIVE_ROOT
        ));
    }
    let modules = Path::new(source).join("usr/lib/modules");
    let kernel = std::fs::read_dir(&modules)
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .any(|entry| entry.path().join("vmlinuz").is_file());
    if !kernel {
        problems.push(format!(
            "{source}: no kernel in {} to install",
            modules.display()
        ));
    }
    problems
}

/// Password hashes libcrypt can use: a well-formed hash with parameters it
/// does not know would be the only way in, and match nothing. Checked with
/// the libcrypt the installed system gets, the hash on standard input.
fn hash_problems(ctx: &Context<'_>) -> Vec<String> {
    const CHECK: &str = r#"my $h = <STDIN>; chomp $h; my $r = crypt("deli", $h); exit(defined $r && $r =~ /^\$/ ? 0 : 1)"#;
    let config = ctx.config;
    [
        ("root.password_hash", config.root.password_hash.as_ref()),
        (
            "admin.password_hash",
            config.admin.as_ref().and_then(|a| a.password_hash.as_ref()),
        ),
    ]
    .into_iter()
    .filter_map(|(key, hash)| Some((key, hash?)))
    .filter(|(_, hash)| {
        let check = Command::new("perl")
            .args(["-e", CHECK])
            .stdin(format!("{}\n", hash.expose()));
        ctx.sys.capture(&check).is_err()
    })
    .map(|(key, _)| format!("{key}: libcrypt cannot use this hash (unknown parameters?)"))
    .collect()
}

/// An account or a group of the system to copy already has this name:
/// useradd would refuse it after the disk is erased.
fn taken(source: &Path, name: &str) -> Vec<String> {
    ["etc/passwd", "etc/group"]
        .iter()
        .filter(|file| {
            std::fs::read_to_string(source.join(file))
                .unwrap_or_default()
                .lines()
                .any(|line| line.split(':').next() == Some(name))
        })
        .map(|file| format!("admin.name: \"{name}\" is already in /{file}: choose another name"))
        .take(1)
        .collect()
}

pub struct Preflight;

impl Step for Preflight {
    fn name(&self) -> &'static str {
        "preflight checks"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let mut problems = Vec::new();
        let facts = ctx.facts;
        let disk_path = &ctx.plan.disk;

        if !facts.uefi {
            problems.push(
                "the machine did not boot in UEFI mode; deli installs systemd-boot, which needs it"
                    .to_owned(),
            );
        }
        match facts.disk(disk_path) {
            None => problems.push(format!("{disk_path}: no such disk")),
            Some(disk) => {
                if facts.boot_disk.as_deref() == Some(disk.path.as_str()) {
                    problems.push(format!("{disk_path}: this is the installation medium"));
                }
                if disk.read_only {
                    problems.push(format!("{disk_path}: read-only"));
                }
                let needed = ctx.plan.min_disk_size();
                if disk.size < needed {
                    problems.push(format!(
                        "{disk_path}: {} is too small, this layout needs at least {}",
                        human(disk.size),
                        human(needed)
                    ));
                }
            }
        }

        if ctx.sys.is_real() {
            problems.extend(source_problems(ctx.source));
            if let Some(admin) = &ctx.config.admin {
                problems.extend(taken(Path::new(ctx.source), &admin.name));
            }
            if !Path::new(ctx.source)
                .join(super::system::LIVE_MANIFEST.trim_start_matches('/'))
                .is_file()
            {
                problems.push(format!(
                    "{}: no {} listing the live-only files to remove",
                    ctx.source,
                    super::system::LIVE_MANIFEST
                ));
            }
            if !Path::new(ctx.source).join("etc/os-release").exists() {
                problems.push(format!(
                    "{}: no system to copy (not booted from the deli ISO?)",
                    ctx.source
                ));
            }
            problems.extend(available_locale_and_keymap(
                ctx.source,
                &ctx.config.system.locale,
                &ctx.config.system.keymap,
            ));
            let zone = Path::new(ctx.source)
                .join("usr/share/zoneinfo")
                .join(&ctx.config.system.timezone);
            if !zone.is_file() {
                problems.push(format!(
                    "system.timezone: unknown zone \"{}\"",
                    ctx.config.system.timezone
                ));
            }
        }

        problems.extend(software_problems(ctx));

        problems.extend(hash_problems(ctx));

        if let Some(admin) = &ctx.config.admin {
            match super::access::admin_keys(ctx, admin) {
                Ok(keys) => {
                    // OpenSSH has the last word on what it will accept.
                    for key in &keys {
                        let check = Command::new("ssh-keygen")
                            .args(["-l", "-f", "/dev/stdin"])
                            .stdin(format!("{key}\n"));
                        if ctx.sys.capture(&check).is_err() {
                            let shown: String = key.chars().take(40).collect();
                            problems.push(format!("admin key rejected by ssh-keygen: {shown}…"));
                        }
                    }
                    ctx.state.admin_keys = keys;
                }
                Err(e) => problems.push(format!("admin keys: {e}")),
            }
        }

        if problems.is_empty() {
            let disk = facts.disk(disk_path).expect("checked above");
            ctx.info(format!("target {}", disk.describe()));
            Ok(())
        } else {
            Err(Error::Invalid(problems))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::report::Reporter;
    use crate::storage::Plan;
    use crate::system::DrySys;

    fn check(facts: &crate::system::probe::Facts) -> Result<()> {
        let config = config("");
        let plan = Plan::new(&config.storage, "/dev/sda");
        let sys = DrySys::default();
        let mut report = Reporter::default();
        let mut ctx = Context {
            config: &config,
            plan: &plan,
            facts,
            sys: &sys,
            report: &mut report,
            source: "/run/rootfsbase",
            state: super::super::State::default(),
        };
        Preflight.run(&mut ctx)
    }

    #[test]
    fn accepts_a_good_machine() {
        check(&facts()).unwrap();
    }

    #[test]
    fn refuses_bios_small_disk_and_boot_medium() {
        let mut facts = facts();
        facts.uefi = false;
        facts.disks[0].size = 4 << 30;
        facts.boot_disk = Some("/dev/sda".into());
        let message = check(&facts).unwrap_err().to_string();
        for needle in ["UEFI", "too small", "installation medium"] {
            assert!(message.contains(needle), "{needle} missing from {message}");
        }
    }

    #[test]
    fn a_hash_libcrypt_refuses_is_refused() {
        let config = config(
            "[root]\npassword_hash = \"$6$testsalt$tJbUl1kXqW33QAR3uSZ526jhi2VR/8b5Oc.fgGcuj1amRP1gtYnGoqbDwnND9jnHaR.tZ1.Uag0nWYDafTUxX0\"\n",
        );
        let sys = DrySys::failing_on("perl");
        let result = with_context(&config, &facts(), &sys, |ctx| Preflight.run(ctx));
        let message = result.unwrap_err().to_string();
        assert!(
            message.contains("root.password_hash: libcrypt"),
            "{message}"
        );
        // The hash went to standard input, which is never shown.
        assert!(!sys.actions().join("\n").contains("testsalt"));
    }

    #[test]
    fn a_sync_needs_exherbo_git() {
        let config = config("[repositories]\nsync = true\n");
        let sys = DrySys::failing_on("curl");
        let result = with_context(&config, &facts(), &sys, |ctx| Preflight.run(ctx));
        let message = result.unwrap_err().to_string();
        assert!(message.contains("cannot be reached"), "{message}");
    }
}
