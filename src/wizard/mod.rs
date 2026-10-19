//! The interactive installer: what the live session runs when it finds no
//! configuration to install unattended. Its screens fill a [`Config`],
//! exactly what a `deli.toml` would hold; the same installation then runs,
//! its progress on screen.

mod form;
mod progress;
mod theme;
mod view;

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;

use crate::config::{Config, NetworkMode};
use crate::error::{Error, Result};
use crate::install;
use crate::network;
use crate::report::Reporter;
use crate::repositories;
use crate::system::probe::{Disk, Facts};
use crate::system::resolvers;
use crate::system::{Command, RealSys, Sys};
use form::{Catalog, Event, Wizard};
use progress::{Choice, Installation};
use theme::{Colors, Theme};

/// A console font with ✓ • … ━ ▌; kbd ships it.
const CONSOLE_FONT: &str = "eurlatgr";
/// The console palette, as `setvtrgb` reads it.
const PALETTE: &str = "/run/deli/palette";

/// How the installer ended.
pub enum Ending {
    Rebooting,
    /// Back to the live session's shell, installed or not.
    Shell,
}

/// The screens, then the installation, until the user reboots into the
/// installed system or leaves for the shell.
pub fn run(source: &str, dry_run: bool, report: &mut Reporter) -> Result<Ending> {
    let facts = install::facts_for(&Config::default(), dry_run)?;
    let sys = RealSys::default();
    // The terminal first: the console is changed only once nothing can fail
    // before it is set back.
    let terminal = open_terminal()?;
    let console = Console::prepare(&sys, dry_run);
    let theme = console.theme();
    let wizard = Wizard::new(catalog(&sys, source, &facts));
    let mut session = Session {
        terminal,
        wizard,
        theme,
        facts,
        source,
        dry_run,
        sys,
    };
    let ending = session.run(report);
    close_terminal();
    console.restore(&session.sys);
    ending
}

struct Session<'a> {
    terminal: DefaultTerminal,
    wizard: Wizard,
    theme: Theme,
    facts: Facts,
    source: &'a str,
    dry_run: bool,
    sys: RealSys,
}

impl Session<'_> {
    fn run(&mut self, report: &mut Reporter) -> Result<Ending> {
        loop {
            if !self.ask()? {
                return Ok(Ending::Shell);
            }
            // The screens may have stayed open for hours: look at the disks
            // again, and erase only the disk that was shown (same name,
            // model, size and serial number).
            if !self.dry_run {
                self.facts = Facts::gather()?;
            }
            if !self.wizard.recheck_disks(candidates(&self.facts)) {
                continue;
            }
            let config = match self.wizard.config(|password| hash(&self.sys, password)) {
                Ok(config) => config,
                Err(e) => {
                    self.wizard.warn(format!("cannot hash the password: {e}"));
                    continue;
                }
            };
            let (choice, erasing) = self.install(&config, report)?;
            match choice {
                Choice::Reboot => return Ok(Ending::Rebooting),
                Choice::Shell => return Ok(Ending::Shell),
                Choice::BackToInstaller => {
                    self.wizard.back_to_summary();
                    if erasing {
                        let target = self.wizard.target();
                        self.wizard.warn(format!("the last attempt had started to erase {target}: installing again erases it"));
                    }
                }
            }
        }
    }

    /// The screens, until the user confirms the installation (`true`) or
    /// leaves (`false`).
    fn ask(&mut self) -> Result<bool> {
        loop {
            self.terminal
                .draw(|frame| view::draw(frame, &self.wizard, self.theme))
                .map_err(terminal_error)?;
            match event::read().map_err(terminal_error)? {
                TermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if self.display_key(key) {
                        continue;
                    }
                    match self.wizard.handle(key) {
                        // Passwords typed next must be the ones the
                        // installed system's console will read.
                        Event::Keymap(keymap) if !self.dry_run => {
                            if let Err(e) = self.sys.capture(&Command::new("loadkeys").arg(&keymap))
                            {
                                self.wizard.warn(format!("the {keymap} layout could not be loaded, the keyboard is unchanged: {e}"));
                            }
                        }
                        Event::CheckNetwork => {
                            // Not while another session installs: its cave
                            // clones over this network.
                            let lock = if self.dry_run {
                                None
                            } else {
                                match install::lock() {
                                    Ok(lock) => Some(lock),
                                    Err(e) => {
                                        self.wizard.cancel_check(e.to_string());
                                        continue;
                                    }
                                }
                            };
                            self.terminal
                                .draw(|frame| view::draw(frame, &self.wizard, self.theme))
                                .map_err(terminal_error)?;
                            let result = self.check_network();
                            drop(lock);
                            self.wizard.set_interfaces(interfaces(&self.sys));
                            self.wizard.checked(result);
                        }
                        Event::Nothing | Event::Keymap(_) => {}
                        Event::Install => return Ok(true),
                        Event::Quit => return Ok(false),
                    }
                }
                TermEvent::Paste(text) => self.wizard.paste(&text),
                _ => {}
            }
        }
    }

    /// Switch this session to the network settings, then see whether they
    /// reach the Internet. A dry run only looks.
    fn check_network(&self) -> std::result::Result<(), String> {
        if !self.dry_run {
            network::apply_to_live(&self.sys, &self.wizard.network())
                .map_err(|e| format!("these settings could not be applied: {e}"))?;
        }
        network::online(&self.sys, Duration::from_secs(20))
    }

    /// Ctrl+T switches to plain mode (ASCII without colour) and back;
    /// Ctrl+L draws the whole screen again, whatever wrote over it.
    fn display_key(&mut self, key: KeyEvent) -> bool {
        if !key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match key.code {
            KeyCode::Char('t') => self.theme.toggle_plain(),
            KeyCode::Char('l') => {
                let _ = self.terminal.clear();
            }
            _ => return false,
        }
        true
    }

    /// Install `config` while showing its progress, then ask what to do;
    /// also whether the disk started being erased. No key interrupts the
    /// installation; if the terminal fails, it still runs to its end and
    /// nothing reboots. A hangup (a lost SSH session) ends the process,
    /// unless it ignores hangups, as SSH logins into the live session have
    /// it (`/etc/profile.d/90-deli-live.sh`).
    fn install(&mut self, config: &Config, report: &mut Reporter) -> Result<(Choice, bool)> {
        let mut run = Installation::new(self.wizard.target(), facts_shown(config, &self.wizard));
        let options = install::Options {
            dry_run: self.dry_run,
            // The wizard asked already.
            yes: true,
            source: self.source.to_owned(),
            quiet: true,
        };
        let (sender, receiver) = mpsc::channel();
        report.listen(sender);
        let _ = self.terminal.clear();
        let facts = &self.facts;
        let mut terminal = true;
        let result = std::thread::scope(|scope| {
            let worker = scope
                .spawn(|| install::run(config, facts, &options, report).map_err(|e| e.to_string()));
            while !worker.is_finished() {
                run.extend(receiver.try_iter());
                if terminal {
                    terminal = self
                        .terminal
                        .draw(|frame| view::draw_install(frame, &run, &self.wizard, self.theme))
                        .is_ok()
                        && keys_ignored(Duration::from_millis(200));
                } else {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
            worker
                .join()
                .unwrap_or_else(|_| Err("the installation stopped unexpectedly".into()))
        });
        report.unlisten();
        run.extend(receiver.try_iter());
        // Without a terminal, nobody chose anything: no reboot.
        if !terminal {
            let outcome = match &result {
                Ok(()) => "the installation finished".to_owned(),
                Err(e) => format!("the installation failed: {e}"),
            };
            return Err(Error::Other(format!(
                "the terminal went away while installing; {outcome} (journals in /run/deli)"
            )));
        }
        run.finish(result);
        let _ = self.terminal.clear();
        // Keys pressed while it ran must not choose for the user.
        while event::poll(Duration::ZERO).unwrap_or(false) {
            let _ = event::read();
        }
        loop {
            self.terminal
                .draw(|frame| view::draw_install(frame, &run, &self.wizard, self.theme))
                .map_err(terminal_error)?;
            if let TermEvent::Key(key) = event::read().map_err(terminal_error)?
                && key.kind == KeyEventKind::Press
                && !self.display_key(key)
                && let Some(choice) = run.handle(key.code)
            {
                return Ok((choice, run.erasing));
            }
        }
    }
}

/// Wait up to `timeout` for a key, and drop it: nothing stops a running
/// installation. `false` once the terminal fails.
fn keys_ignored(timeout: Duration) -> bool {
    match event::poll(timeout) {
        Ok(true) => event::read().is_ok(),
        Ok(false) => true,
        Err(_) => false,
    }
}

/// What the end screen tells about the new system.
fn facts_shown(config: &Config, wizard: &Wizard) -> Vec<(&'static str, String)> {
    let address = match config.network.mode {
        NetworkMode::Static => config.network.address.clone().unwrap_or_default(),
        NetworkMode::Dhcp => "from DHCP: your DHCP server knows it".into(),
    };
    let ssh = if config.ssh.enable {
        "enabled"
    } else {
        "disabled: console logins only"
    };
    vec![
        ("Host name", config.system.hostname.clone()),
        (
            "User name",
            config
                .admin
                .as_ref()
                .map(|a| a.name.clone())
                .unwrap_or_default(),
        ),
        ("Signs in with", wizard.access()),
        ("SSH server", ssh.into()),
        ("Address", address),
    ]
}

fn terminal_error(e: std::io::Error) -> Error {
    Error::io("cannot draw on the terminal", e)
}

fn open_terminal() -> Result<DefaultTerminal> {
    match ratatui::try_init() {
        Ok(terminal) => {
            // Pasted text arrives whole, not as keys that could press a
            // button.
            let _ = execute!(std::io::stdout(), EnableBracketedPaste);
            Ok(terminal)
        }
        // It may have failed after switching the terminal to raw mode.
        Err(e) => {
            ratatui::restore();
            Err(terminal_error(e))
        }
    }
}

fn close_terminal() {
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
}

/// What the installer changed on the machine's own console: its font,
/// kept afterwards since it only adds glyphs; its palette and whether
/// kernel messages print on it, both set back when it leaves.
struct Console {
    /// The eurlatgr font is loaded; `None`: not a virtual console, or a dry
    /// run, which leaves it alone.
    font: Option<bool>,
    palette: bool,
    muted: bool,
}

impl Console {
    fn prepare(sys: &RealSys, dry_run: bool) -> Self {
        if !on_virtual_console() || dry_run {
            return Self {
                font: None,
                palette: false,
                muted: false,
            };
        }
        let ok = |command: Command| sys.capture(&command).is_ok();
        let font = ok(Command::new("setfont").arg(CONSOLE_FONT));
        let palette = sys.write(PALETTE, &theme::console_palette(), 0o644).is_ok()
            && ok(Command::new("setvtrgb").arg(PALETTE));
        // Kernel messages printed on the console would scroll the screens
        // away: keep them in the kernel log while the installer shows.
        let muted = ok(Command::new("dmesg").arg("--console-off"));
        Self {
            font: Some(font),
            palette,
            muted,
        }
    }

    /// The machine's console draws the palette and the symbols when it
    /// took them; another terminal says what it can.
    fn theme(&self) -> Theme {
        match self.font {
            Some(font) if self.palette => Theme::new(Colors::Console, font),
            // The console's own colours would put magenta and blue where
            // the palette has greys: plain mode is clearer.
            Some(_) => Theme::plain(Colors::Console),
            None => match terminal_colors() {
                Some(colors) => Theme::new(colors, utf8_locale() || modern_terminal()),
                // A serial line or an unknown terminal: plain mode.
                None => Theme::plain(Colors::Indexed),
            },
        }
    }

    fn restore(&self, sys: &RealSys) {
        if self.palette {
            let _ = sys.capture(&Command::new("setvtrgb").arg("vga"));
        }
        if self.muted {
            let _ = sys.capture(&Command::new("dmesg").arg("--console-on"));
        }
    }
}

/// Terminals that draw true colour and UTF-8 whatever the environment
/// says: SSH passes on `TERM`, rarely `COLORTERM` or the locale.
const MODERN_TERMINALS: [&str; 5] = [
    "xterm-ghostty",
    "xterm-kitty",
    "alacritty",
    "wezterm",
    "foot",
];

fn modern_terminal() -> bool {
    std::env::var("TERM").is_ok_and(|term| MODERN_TERMINALS.contains(&term.as_str()))
}

/// The colours the terminal announces: true colour, 256 colours, or none
/// it can be trusted with.
fn terminal_colors() -> Option<Colors> {
    let var = |name| std::env::var(name).unwrap_or_default();
    if matches!(var("COLORTERM").as_str(), "truecolor" | "24bit") || modern_terminal() {
        Some(Colors::TrueColor)
    } else if var("TERM").contains("256color") {
        Some(Colors::Indexed)
    } else {
        None
    }
}

/// Whether the terminal draws symbols beyond ASCII: a UTF-8 locale.
fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| {
            let v = v.to_uppercase();
            v.contains("UTF-8") || v.contains("UTF8")
        })
}

/// Whether standard input is one of this machine's virtual consoles
/// (`/dev/ttyN`), whose font can be changed: not SSH, not a serial line.
fn on_virtual_console() -> bool {
    std::fs::read_link("/proc/self/fd/0").is_ok_and(|path| {
        path.to_str()
            .and_then(|p| p.strip_prefix("/dev/tty"))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    })
}

/// SHA-512 crypt hash, the password passed on standard input only.
fn hash(sys: &RealSys, password: &str) -> Result<String> {
    let out = sys.capture(
        &Command::new("openssl")
            .args(["passwd", "-6", "-stdin"])
            .stdin(format!("{password}\n")),
    )?;
    Ok(out.trim().to_owned())
}

/// What the system about to be copied (`root`) and this machine offer.
fn catalog(sys: &RealSys, root: &str, facts: &Facts) -> Catalog {
    let root = Path::new(root);
    let mut keymaps = Vec::new();
    keymap_names(&root.join("usr/share/keymaps/i386"), &mut keymaps);
    keymaps.sort();
    keymaps.dedup();
    if keymaps.is_empty() {
        keymaps.push("us".into());
    }

    let mut locales: Vec<String> = std::fs::read_dir(root.join("usr/share/i18n/locales"))
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| is_locale_name(name))
        .map(|name| format!("{name}.UTF-8"))
        .collect();
    for always in crate::steps::STAGE_LOCALES {
        if !locales.iter().any(|l| l == always) {
            locales.push(always.into());
        }
    }
    locales.sort();

    let mut zones: Vec<String> =
        std::fs::read_to_string(root.join("usr/share/zoneinfo/zone1970.tab"))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split('\t').nth(2))
            .map(str::to_owned)
            .collect();
    zones.sort();
    zones.insert(0, "UTC".into());

    Catalog {
        keymaps,
        locales,
        zones,
        disks: candidates(facts),
        interfaces: interfaces(sys),
        uefi: facts.uefi,
        dnssec: resolvers::pass_dnssec(),
        repositories: repositories::Index::read(root),
        ssh_password: std::fs::read_to_string(crate::launch::SSH_PASSWORD)
            .ok()
            .map(|p| p.trim().to_owned()),
    }
}

/// `ip -brief address`: this session's interfaces and their addresses.
fn interfaces(sys: &RealSys) -> String {
    sys.capture(
        &Command::new("ip").args(["-brief", "-4", "address", "show", "scope", "global", "up"]),
    )
    .unwrap_or_default()
}

fn candidates(facts: &Facts) -> Vec<Disk> {
    facts.candidate_disks().into_iter().cloned().collect()
}

/// `fr_FR`, `ast_ES`: the locales glibc can build, not its helper files.
fn is_locale_name(name: &str) -> bool {
    match name.split_once('_') {
        Some((language, country)) => {
            (2..=3).contains(&language.len())
                && language.chars().all(|c| c.is_ascii_lowercase())
                && country.len() == 2
                && country.chars().all(|c| c.is_ascii_uppercase())
        }
        None => false,
    }
}

fn keymap_names(dir: &Path, names: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "include" {
                keymap_names(&path, names);
            }
        } else if let Some(map) = name
            .strip_suffix(".map.gz")
            .or_else(|| name.strip_suffix(".map"))
        {
            names.push(map.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_names() {
        for good in ["fr_FR", "ast_ES", "en_US"] {
            assert!(is_locale_name(good), "{good}");
        }
        for bad in ["i18n", "translit_combining", "fr_FR@euro", "POSIX", "en_us"] {
            assert!(!is_locale_name(bad), "{bad}");
        }
    }
}
