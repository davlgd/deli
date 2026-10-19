//! Who can log in, and how.

use std::fmt::Write as _;

use super::{Context, Step};
use crate::config::Admin;
use crate::error::{Error, Result};
use crate::system::{Command, Sys, TARGET, target_path};

/// Supplementary groups for an administrator with sudo, when the system
/// has them.
const ADMIN_GROUPS: [&str; 4] = ["wheel", "adm", "systemd-journal", "users"];
/// Those of an account without sudo: Exherbo's PAM lets only wheel use su,
/// which is how it reaches root; wheel grants nothing in sudoers.
const USER_GROUPS: [&str; 2] = ["wheel", "users"];

pub struct Accounts;

impl Step for Accounts {
    fn name(&self) -> &'static str {
        "creating accounts"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let sys = ctx.sys;
        if let Some(hash) = &ctx.config.root.password_hash {
            set_password(sys, "root", hash.expose())?;
            ctx.info("root: password set");
        } else {
            // Whatever the system copied had, not on the stage's say-so.
            set_password(sys, "root", "!")?;
            ctx.info("root: locked");
        }

        let Some(admin) = ctx.config.admin.clone() else {
            return Ok(());
        };
        let wanted: &[&str] = if admin.sudo {
            &ADMIN_GROUPS
        } else {
            &USER_GROUPS
        };
        let groups = if sys.is_real() {
            let group_file = std::fs::read_to_string(target_path("/etc/group"))
                .map_err(|e| Error::io("cannot read the target's /etc/group", e))?;
            existing_groups(wanted, &group_file)
        } else {
            wanted.to_vec()
        };
        sys.run(&Command::new("useradd").args([
            "--root",
            TARGET,
            "--create-home",
            "--user-group",
            "--shell",
            "/bin/bash",
            "--groups",
            &groups.join(","),
            &admin.name,
        ]))?;
        // useradd leaves "!" (locked), which sshd also treats as "no key
        // logins". "*" means no password, keys still accepted.
        let password = admin.password_hash.as_ref().map_or("*", |h| h.expose());
        set_password(sys, &admin.name, password)?;

        if admin.sudo {
            // sudo silently ignores a drop-in that is group- or
            // world-writable.
            let rule = if admin.password_hash.is_some() {
                "ALL=(ALL:ALL) ALL"
            } else {
                "ALL=(ALL:ALL) NOPASSWD: ALL"
            };
            sys.write(
                &target_path("/etc/sudoers.d/10-deli-admin"),
                &format!("{} {rule}\n", admin.name),
                0o440,
            )?;
        }

        let keys = ctx.state.admin_keys.clone();
        if !keys.is_empty() {
            let home = format!("/home/{}", admin.name);
            sys.mkdir_p(&target_path(&format!("{home}/.ssh")), 0o700)?;
            let mut text = String::new();
            for key in &keys {
                writeln!(text, "{key}").unwrap();
            }
            sys.write(
                &target_path(&format!("{home}/.ssh/authorized_keys")),
                &text,
                0o600,
            )?;
            sys.run(
                &Command::new("chown")
                    .args([
                        "-R",
                        &format!("{0}:{0}", admin.name),
                        &format!("{home}/.ssh"),
                    ])
                    .in_target(),
            )?;
        }
        ctx.info(format!(
            "{}: {}, groups {}, {} SSH key(s)",
            admin.name,
            if admin.sudo { "sudo" } else { "no sudo" },
            groups.join(","),
            keys.len()
        ));
        Ok(())
    }
}

/// Set a crypt(3) hash through stdin: never in argv, logs or errors.
fn set_password(sys: &dyn Sys, user: &str, hash: &str) -> Result<()> {
    sys.run(
        &Command::new("chpasswd")
            .args(["--root", TARGET, "--encrypted"])
            .stdin(format!("{user}:{hash}\n")),
    )
}

/// The administrator's keys: from the configuration, plus GitHub's.
pub fn admin_keys(ctx: &Context<'_>, admin: &Admin) -> Result<Vec<String>> {
    let mut keys = admin.authorized_keys.clone();
    let Some(user) = &admin.github else {
        return Ok(keys);
    };
    let published = ctx.sys.capture(&Command::new("curl").args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "20",
        &format!("https://github.com/{user}.keys"),
    ]))?;
    let before = keys.len();
    keys.extend(
        published
            .lines()
            .map(str::trim)
            .filter(|l| crate::sshkey::is_valid(l))
            .map(|k| format!("{k} github:{user}")),
    );
    if ctx.sys.is_real() && keys.len() == before {
        return Err(Error::Other(format!(
            "GitHub publishes no usable SSH key for {user}"
        )));
    }
    Ok(keys)
}

fn existing_groups<'a>(wanted: &[&'a str], group_file: &str) -> Vec<&'a str> {
    wanted
        .iter()
        .copied()
        .filter(|g| group_file.lines().any(|l| l.split(':').next() == Some(g)))
        .collect()
}

pub fn sshd_config(ctx: &Context<'_>) -> String {
    let config = ctx.config;
    let has_keys = config
        .admin
        .as_ref()
        .is_some_and(|a| !a.authorized_keys.is_empty() || a.github.is_some());
    let passwords = config.ssh.password_authentication.unwrap_or(!has_keys);
    let root_login = match (&config.admin, &config.root.password_hash) {
        (Some(_), _) => "no",
        (None, Some(_)) if passwords => "yes",
        (None, _) => "prohibit-password",
    };
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    format!(
        "# Written by deli.\nPermitRootLogin {root_login}\nPasswordAuthentication {}\nKbdInteractiveAuthentication no\nX11Forwarding no\nMaxAuthTries 4\nLoginGraceTime 30\n",
        yes_no(passwords)
    )
}

pub struct Ssh;

impl Step for Ssh {
    fn name(&self) -> &'static str {
        "configuring SSH"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        if !ctx.config.ssh.enable {
            ctx.info("disabled");
            return Ok(());
        }
        ctx.sys.write(
            &target_path("/etc/ssh/sshd_config.d/10-deli.conf"),
            &sshd_config(ctx),
            0o644,
        )?;
        // Host keys are generated on first boot by sshd-keygen.service: the
        // live session's never reach the disk (they live in its overlay).
        ctx.sys.run(&Command::new("systemctl").args([
            &format!("--root={TARGET}"),
            "enable",
            "sshd.service",
        ]))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::config::Secret;

    #[test]
    fn admin_with_keys_and_no_password() {
        let config = config("");
        let actions = actions(&config, &facts(), |ctx| {
            ctx.state.admin_keys = vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIM7w6tvHPjMHe2grkjvMyevSUvyZE/gsp1Xnad41MMU0 ops@example".into()];
            Accounts.run(ctx)
        });
        assert!(contains(
            &actions,
            "useradd --root /run/deli/target --create-home --user-group --shell /bin/bash --groups wheel,adm,systemd-journal,users ops"
        ));
        assert!(contains(
            &actions,
            "chpasswd --root /run/deli/target --encrypted"
        ));
        assert!(!contains(&actions, "ops:*"));
        // root locked, then the administrator's password.
        assert_eq!(
            actions.iter().filter(|a| a.contains("chpasswd")).count(),
            2,
            "{actions:?}"
        );
        assert!(contains(
            &actions,
            "write /run/deli/target/etc/sudoers.d/10-deli-admin (440)"
        ));
        assert!(contains(&actions, "| ops ALL=(ALL:ALL) NOPASSWD: ALL"));
        assert!(contains(
            &actions,
            "write /run/deli/target/home/ops/.ssh/authorized_keys (600)"
        ));
        assert!(contains(&actions, "-- chown -R ops:ops /home/ops/.ssh"));
    }

    #[test]
    fn password_means_sudo_asks() {
        let mut config = config("");
        config.admin.as_mut().unwrap().password_hash = Some(Secret::new(
            "$6$testsalt$tJbUl1kXqW33QAR3uSZ526jhi2VR/8b5Oc.fgGcuj1amRP1gtYnGoqbDwnND9jnHaR.tZ1.Uag0nWYDafTUxX0",
        ));
        let actions = actions(&config, &facts(), |ctx| Accounts.run(ctx));
        assert!(contains(&actions, "| ops ALL=(ALL:ALL) ALL"));
    }

    #[test]
    fn without_sudo_an_ordinary_account_and_root_s_password() {
        let config = config(
            "[root]\npassword_hash = \"$6$testsalt$tJbUl1kXqW33QAR3uSZ526jhi2VR/8b5Oc.fgGcuj1amRP1gtYnGoqbDwnND9jnHaR.tZ1.Uag0nWYDafTUxX0\"\n",
        );
        let mut config = config;
        config.admin.as_mut().unwrap().sudo = false;
        config.validate().unwrap();
        let actions = actions(&config, &facts(), |ctx| Accounts.run(ctx));
        // wheel: su asks for root's password, nothing more.
        assert!(
            contains(&actions, "--groups wheel,users ops"),
            "{actions:?}"
        );
        assert!(!contains(&actions, "sudoers"), "{actions:?}");
    }

    #[test]
    fn groups_missing_on_the_system_are_skipped() {
        assert_eq!(
            existing_groups(&ADMIN_GROUPS, "root:x:0:\nwheel:x:10:\nusers:x:100:\n"),
            ["wheel", "users"]
        );
    }

    #[test]
    fn sshd_keys_only_by_default() {
        let config = config("");
        actions(&config, &facts(), |ctx| {
            let text = sshd_config(ctx);
            assert!(text.contains("PermitRootLogin no\n"));
            assert!(text.contains("PasswordAuthentication no\n"));
            Ok(())
        });
    }
}
