//! The interactive installer's model: screens of controls, key presses in, a
//! configuration out. Nothing here touches the terminal or the machine, so
//! tests drive it with key presses and check the configuration it produces.
//!
//! Keys: Tab and Shift+Tab move between controls (fields, then buttons), and
//! so do Up and Down, except in the list of disks, where they choose one.
//! Left and Right move the cursor in a text, change a choice, or move between
//! buttons. Space changes a choice, opens a list or presses a button. Enter
//! activates the focused control: a button acts, a list opens, any other
//! field hands the focus on, to the main button after the last field. Esc
//! closes a dialog without changing anything, or goes back a screen.

use std::collections::BTreeMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Admin, Config, Filesystem, Network, NetworkMode, Secret};
use crate::error::Result;
use crate::repositories::Index;
use crate::storage::{Plan, human};
use crate::system::probe::Disk;

/// What the machine offers to choose from, read once before the first screen.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub keymaps: Vec<String>,
    pub locales: Vec<String>,
    pub zones: Vec<String>,
    /// Candidate disks only: never the installation medium.
    pub disks: Vec<Disk>,
    /// `ip -brief address`, shown on the network screen.
    pub interfaces: String,
    pub uefi: bool,
    /// This network's resolvers pass DNSSEC on: validation is required by
    /// default.
    pub dnssec: bool,
    /// The repositories the system to copy uses, and those offered to add.
    pub repositories: Index,
    /// root's password for SSH into this live session, when it has one.
    pub ssh_password: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Answer {
    Keymap,
    /// A field to try the layout in, not part of the configuration.
    KeyboardTest,
    Locale,
    Timezone,
    Network,
    Address,
    Gateway,
    Dns,
    Dnssec,
    Disk,
    Filesystem,
    Swap,
    Hostname,
    Admin,
    Password,
    PasswordAgain,
    Administration,
    RootPassword,
    RootPasswordAgain,
    PublicKey,
    Github,
    Ssh,
    Repositories,
    Sync,
    Packages,
}

impl Answer {
    pub fn label(self) -> &'static str {
        match self {
            Self::Keymap => "Layout",
            Self::KeyboardTest => "Try it",
            Self::Locale => "Language",
            Self::Timezone => "Time zone",
            Self::Network => "Configuration",
            Self::Address => "Address",
            Self::Gateway => "Gateway",
            Self::Dns => "DNS servers",
            Self::Dnssec => "DNSSEC",
            Self::Disk => "Disk",
            Self::Filesystem => "Filesystem",
            Self::Swap => "Swap",
            Self::Hostname => "Host name",
            Self::Admin => "User name",
            Self::Password => "Password",
            Self::PasswordAgain => "Confirm password",
            Self::Administration => "Administration",
            Self::RootPassword => "Root password",
            Self::RootPasswordAgain => "Confirm root",
            Self::PublicKey => "SSH public key",
            Self::Github => "GitHub user",
            Self::Ssh => "SSH server",
            Self::Repositories => "Add",
            Self::Sync => "Sync",
            Self::Packages => "Packages",
        }
    }

    pub fn help(self, network: &str) -> &'static str {
        match self {
            Self::Keymap => "Enter opens the list; type to search it.",
            Self::KeyboardTest => "Type a few keys to check the layout. Not saved.",
            Self::Locale => "Language of messages, dates and numbers.",
            Self::Timezone => "Search by city, such as Paris or New_York.",
            Self::Network => "DHCP suits most networks.",
            Self::Address => "With its prefix length, such as 192.0.2.10/24.",
            Self::Gateway => "The router's address, such as 192.0.2.1.",
            Self::Dns if network == "static" => "One or more, separated by spaces.",
            Self::Dns => "Separated by spaces; leave empty to use those DHCP gives.",
            Self::Dnssec => {
                "Chosen after asking this network's resolvers: Required when they pass DNSSEC on, Off when they strip it. Allow downgrade suits a server that will move to other networks."
            }
            Self::Disk => "Up and Down choose the disk. All of it will be erased.",
            Self::Filesystem => "btrfs adds compression and snapshots; ext4 and xfs are classic.",
            Self::Swap => "\"none\", or a size such as 4G.",
            Self::Hostname => "Letters, digits and hyphens, such as web01.",
            Self::Admin => "Lowercase letters, digits, - and _.",
            Self::Password => "Optional with an SSH key; sudo asks for it.",
            Self::PasswordAgain => "The same password again.",
            Self::Administration => {
                "sudo: this account administers, root stays locked. Root password: root gets one to administer with, through su, and this account no sudo."
            }
            Self::RootPassword => "root's own; it never logs in over SSH.",
            Self::RootPasswordAgain => "The same root password again.",
            Self::PublicKey => "Optional: an OpenSSH public key line; paste it.",
            Self::Github => "Optional: DELI installs the public keys published there.",
            Self::Ssh => "Disabled leaves console logins only.",
            Self::Repositories => {
                "All of them unless left out: Enter opens the list, Space takes or leaves one, Ctrl+A all or none."
            }
            Self::Sync => {
                "Now: cave sync and cave generate-metadata end the installation, a few minutes more. Later: run them yourself."
            }
            Self::Packages => {
                "Optional, separated by spaces, such as htop or app-editors/vim: added to world, not built now."
            }
        }
    }

    /// The title of the list a field is chosen from.
    pub fn list_title(self) -> &'static str {
        match self {
            Self::Keymap => "Keyboard layout",
            Self::Repositories => "Exherbo repositories",
            answer => answer.label(),
        }
    }

    /// The field a configuration key is about.
    fn for_key(key: &str) -> Option<Self> {
        Some(match key {
            "system.keymap" => Self::Keymap,
            "system.locale" => Self::Locale,
            "system.timezone" => Self::Timezone,
            "system.hostname" => Self::Hostname,
            "network.address" => Self::Address,
            "network.gateway" => Self::Gateway,
            "network.dns" => Self::Dns,
            "network.dnssec" => Self::Dnssec,
            "storage.disk" => Self::Disk,
            "storage.swap" => Self::Swap,
            "admin.name" => Self::Admin,
            "admin.password_hash" => Self::Password,
            "admin.sudo" => Self::Administration,
            "root.password_hash" => Self::RootPassword,
            "admin.github" => Self::Github,
            "repositories.enable" => Self::Repositories,
            "packages.world" => Self::Packages,
            key if key.starts_with("admin.authorized_keys") => Self::PublicKey,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Welcome,
    Keyboard,
    Region,
    Network,
    Storage,
    Identity,
    Access,
    Software,
    Summary,
}

impl Screen {
    pub const ALL: [Self; 9] = [
        Self::Welcome,
        Self::Keyboard,
        Self::Region,
        Self::Network,
        Self::Storage,
        Self::Identity,
        Self::Access,
        Self::Software,
        Self::Summary,
    ];

    /// The screen that asks for a configuration key (`network.dns`).
    pub fn asking(key: &str) -> Self {
        let section = key.split(['.', '[']).next().unwrap_or_default();
        match (section, key) {
            (_, "system.keymap") => Self::Keyboard,
            (_, "system.hostname") => Self::Identity,
            ("system", _) => Self::Region,
            ("network", _) => Self::Network,
            ("storage", _) => Self::Storage,
            ("admin" | "root" | "ssh", _) => Self::Access,
            ("repositories" | "packages", _) => Self::Software,
            _ => Self::Summary,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome",
            Self::Keyboard => "Keyboard",
            Self::Region => "Language and time",
            Self::Network => "Network",
            Self::Storage => "Storage",
            Self::Identity => "Host name",
            Self::Access => "Administrator",
            Self::Software => "Software",
            Self::Summary => "Summary",
        }
    }

    /// The first line of the screen.
    pub fn heading(self) -> &'static str {
        match self {
            Self::Welcome => "Install Exherbo Linux on this server",
            Self::Keyboard => "Keyboard layout",
            Self::Software => "Repositories and packages",
            screen => screen.title(),
        }
    }

    pub fn intro(self) -> &'static str {
        match self {
            Self::Welcome => {
                "A server system with Linux 6.18 LTS, systemd and SSH, copied from this ISO and set up from your answers."
            }
            Self::Keyboard => {
                "Applied at once, so the passwords you type next are the ones the console expects. The installed system keeps it."
            }
            Self::Region => "The installed system's language and clock.",
            Self::Network => {
                "How the installed system joins the network. Continue applies it to this live session too, to check the connection."
            }
            Self::Storage => "DELI uses the whole disk you choose. Everything on it is erased.",
            Self::Identity => "The name this server gives itself on the network.",
            Self::Access => {
                "Your account, which administers with sudo while root stays locked, unless root gets a password instead. Give a password, a GitHub user or an SSH key. With a key, SSH accepts keys only."
            }
            Self::Software => {
                "cave installs packages from repositories: those of this system's packages are always there, add others now or later. Packages listed here join the world set, for your first cave resolve -x world to build."
            }
            Self::Summary => {
                "Check your answers. Back returns to any screen; Install asks once more."
            }
        }
    }

    /// More about the screen, where the console is wide enough to say it.
    pub fn about(self) -> &'static str {
        match self {
            Self::Welcome => {
                "Each answer can be changed until the last screen. Alt+F2 opens a shell."
            }
            Self::Keyboard => "Over SSH, this sets the installed console's layout, not yours.",
            Self::Region => "Search a list by typing: a city finds its time zone.",
            Self::Network => {
                "A static address needs its prefix, a gateway and at least one DNS server. DNSSEC validation needs resolvers that pass its signatures on, which many ISP boxes strip: DELI asked this network's and chose for you. Over SSH, a new address ends this connection."
            }
            Self::Storage => "The partitions are those of a GPT disk booted by systemd-boot.",
            Self::Identity => "Other machines find it by this name only if your DNS knows it.",
            Self::Access => {
                "GitHub keys are fetched before the disk is touched. root never logs in over SSH: log in as this account, then use sudo or su."
            }
            Self::Software => {
                "Repositories: the project's, its developers' and a few others' (CleverCloud, platypus); later, cave resolve -x repository/<name> adds any Exherbo lists. Nothing is built during the installation."
            }
            Self::Summary => {
                "The configuration is kept in /var/log/deli/deli.toml, without secrets."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Back,
    Start,
    Continue,
    Install,
    Shell,
}

impl Button {
    pub fn label(self) -> &'static str {
        match self {
            Self::Back => "Back",
            Self::Start => "Start",
            Self::Continue => "Continue",
            Self::Install => "Install",
            Self::Shell => "Live shell",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub value: String,
    pub label: String,
}

impl Item {
    fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }

    fn plain(value: &str) -> Self {
        Self::new(value, value)
    }
}

#[derive(Debug, Clone)]
pub enum Kind {
    Text,
    Secret,
    /// A few choices, shown side by side.
    Radio(Vec<Item>),
    /// A long list, chosen from a dialog with a search.
    Picker,
    /// A long list to choose several from, in the same dialog.
    Choices,
    /// The disks, listed in place.
    Disks(Vec<Item>),
}

#[derive(Debug, Clone)]
pub struct Field {
    pub answer: Answer,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub enum Control {
    Field(Field),
    Button(Button),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// Choosing `answer` from its list: what was typed to search it, the
    /// highlighted line among the matches, and for several choices, those
    /// made so far.
    Picker {
        answer: Answer,
        query: String,
        cursor: usize,
        chosen: Option<Vec<String>>,
    },
    /// The last question before erasing the disk; `true` when the install
    /// button is selected.
    Confirm { install: bool },
    /// These network settings reach no Internet, for `reason`; `true` when
    /// going on without it is selected.
    Offline { reason: String, proceed: bool },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Nothing,
    /// A keyboard layout was chosen: load it on the console.
    Keymap(String),
    /// The network settings hold: switch this session to them and say,
    /// with [`Wizard::checked`], whether they reach the Internet.
    CheckNetwork,
    /// Confirmed: erase the disk and install.
    Install,
    /// Leave the installer for the live shell.
    Quit,
}

/// Something to fix: under its field, or for the whole screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub answer: Option<Answer>,
    pub message: String,
}

impl Issue {
    fn on(answer: Answer, message: impl Into<String>) -> Self {
        Self {
            answer: Some(answer),
            message: message.into(),
        }
    }
}

/// A line of the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Heading(&'static str),
    Pair(&'static str, String),
    /// What the user must not miss.
    Warning(String),
}

#[derive(Clone)]
pub struct Wizard {
    catalog: Catalog,
    values: BTreeMap<Answer, String>,
    screen: usize,
    /// Index of the focused control among [`Wizard::controls`].
    focus: usize,
    /// Cursor of the focused text field, in characters.
    cursor: usize,
    modal: Option<Modal>,
    issues: Vec<Issue>,
    /// First summary line shown.
    scroll: usize,
    /// Whether the network settings reached the Internet, once checked.
    online: Option<bool>,
    /// The network settings are being checked.
    checking: bool,
}

/// Well formed, matches no password a person types: drafts are validated
/// with it, and only ever installed with the real hash.
const DRAFT_HASH: &str = "$6$deli.draft$00000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

/// openssl passwd reads lines of up to 1024 bytes: never let it hash less
/// than what was typed.
const MAX_PASSWORD: usize = 512;

/// Each password and the field that confirms it.
const PASSWORDS: [(Answer, Answer); 2] = [
    (Answer::Password, Answer::PasswordAgain),
    (Answer::RootPassword, Answer::RootPasswordAgain),
];

impl Wizard {
    pub fn new(catalog: Catalog) -> Self {
        let disk = catalog
            .disks
            .first()
            .map(|d| d.path.clone())
            .unwrap_or_default();
        let mut values = [
            (Answer::Keymap, "us"),
            (Answer::Locale, "en_US.UTF-8"),
            (Answer::Timezone, "UTC"),
            (Answer::Network, "dhcp"),
            (Answer::Dnssec, if catalog.dnssec { "yes" } else { "no" }),
            (Answer::Disk, &disk),
            (Answer::Filesystem, "btrfs"),
            (Answer::Swap, "none"),
            (Answer::Hostname, "exherbo"),
            (Answer::Ssh, "yes"),
            (Answer::Administration, "sudo"),
            (Answer::Sync, "yes"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_owned()))
        .collect::<BTreeMap<_, _>>();
        // Every repository offered, to leave out those not wanted.
        let offered: Vec<&str> = catalog
            .repositories
            .offered()
            .map(|r| r.name.as_str())
            .collect();
        values.insert(Answer::Repositories, offered.join(" "));
        let mut wizard = Self {
            catalog,
            values,
            screen: 0,
            focus: 0,
            cursor: 0,
            modal: None,
            issues: Vec::new(),
            scroll: 0,
            online: None,
            checking: false,
        };
        wizard.set_focus(wizard.primary());
        wizard
    }

    pub fn screen(&self) -> Screen {
        Screen::ALL[self.screen]
    }

    /// `(index, count)` of the current screen, from 1.
    pub fn step(&self) -> (usize, usize) {
        (self.screen + 1, Screen::ALL.len())
    }

    /// Screens before the current one have been checked and passed.
    pub fn done(&self, screen: Screen) -> bool {
        Screen::ALL.iter().position(|s| *s == screen) < Some(self.screen)
    }

    pub fn focus(&self) -> usize {
        self.focus
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn interfaces(&self) -> &str {
        &self.catalog.interfaces
    }

    /// This session's interfaces, once its network changed.
    pub fn set_interfaces(&mut self, interfaces: String) {
        self.catalog.interfaces = interfaces;
    }

    pub fn checking(&self) -> bool {
        self.checking
    }

    /// The network settings, to check them.
    pub fn network(&self) -> Network {
        self.draft().network
    }

    /// The network settings reach no Internet, and the user goes on.
    fn offline(&self) -> bool {
        self.online == Some(false)
    }

    /// root's password for SSH into this live session, when it has one.
    pub fn ssh_password(&self) -> Option<&str> {
        self.catalog.ssh_password.as_deref()
    }

    /// The repositories the installed system will use.
    pub fn enabled_repositories(&self) -> &[String] {
        &self.catalog.repositories.enabled
    }

    /// The values chosen for a field of several choices.
    fn chosen(&self, answer: Answer) -> Vec<String> {
        self.value(answer)
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    pub fn value(&self, answer: Answer) -> &str {
        self.values.get(&answer).map_or("", String::as_str)
    }

    fn set(&mut self, answer: Answer, value: impl Into<String>) {
        self.values.insert(answer, value.into());
        // Whatever was wrong with it may be fixed: check again on Continue.
        self.issues.retain(|i| i.answer != Some(answer));
    }

    /// The password typed in `answer`, if any.
    fn password(&self, answer: Answer) -> Option<&str> {
        Some(self.value(answer)).filter(|p| !p.is_empty())
    }

    /// root gets a password rather than the account sudo.
    fn root_password_asked(&self) -> bool {
        self.value(Answer::Administration) == "root"
    }

    /// root's password, when asked for.
    fn root_password(&self) -> Option<&str> {
        self.password(Answer::RootPassword)
            .filter(|_| self.root_password_asked())
    }

    /// `Some(true)` when a password and its confirmation `again` match, once
    /// the confirmation is typed.
    pub fn passwords_match(&self, again: Answer) -> Option<bool> {
        let (first, _) = PASSWORDS.iter().find(|(_, a)| *a == again)?;
        let typed = self.value(again);
        (!typed.is_empty()).then(|| typed == self.value(*first))
    }

    /// Why this machine cannot be installed, if it cannot.
    pub fn blockers(&self) -> Vec<&'static str> {
        let mut blockers = Vec::new();
        if !self.catalog.uefi {
            blockers.push("This machine started in legacy BIOS mode. DELI installs systemd-boot, which needs UEFI: restart the machine in UEFI mode.");
        }
        if self.catalog.disks.is_empty() {
            blockers.push("No disk to install on: DELI needs a fixed disk other than the installation medium.");
        }
        blockers
    }

    /// The disks DELI may install on, for the welcome screen.
    pub fn disks(&self) -> &[Disk] {
        &self.catalog.disks
    }

    pub fn uefi(&self) -> bool {
        self.catalog.uefi
    }

    /// The buttons of this screen, main one last, and whether each can be
    /// pressed.
    pub fn buttons(&self) -> Vec<(Button, bool)> {
        match self.screen() {
            Screen::Welcome => vec![
                (Button::Shell, true),
                (Button::Start, self.blockers().is_empty()),
            ],
            Screen::Summary => vec![(Button::Back, true), (Button::Install, true)],
            _ => vec![(Button::Back, true), (Button::Continue, true)],
        }
    }

    /// The items of a list field.
    pub fn items(&self, answer: Answer) -> Vec<Item> {
        let plain = |list: &[String]| list.iter().map(|v| Item::plain(v)).collect();
        match answer {
            Answer::Keymap => plain(&self.catalog.keymaps),
            Answer::Locale => plain(&self.catalog.locales),
            Answer::Timezone => plain(&self.catalog.zones),
            Answer::Disk => self
                .catalog
                .disks
                .iter()
                .map(|d| Item::new(&d.path, d.describe()))
                .collect(),
            Answer::Network => vec![
                Item::new("dhcp", "DHCP"),
                Item::new("static", "Static address"),
            ],
            Answer::Filesystem => vec![
                Item::plain("btrfs"),
                Item::plain("ext4"),
                Item::plain("xfs"),
            ],
            Answer::Ssh => vec![Item::new("yes", "Enabled"), Item::new("no", "Disabled")],
            Answer::Administration => vec![
                Item::new("sudo", "sudo"),
                Item::new("root", "Root password"),
            ],
            Answer::Dnssec => vec![
                Item::new("no", "Off"),
                Item::new("allow-downgrade", "Allow downgrade"),
                Item::new("yes", "Required"),
            ],
            Answer::Repositories => self
                .catalog
                .repositories
                .offered()
                .map(|r| Item::new(&r.name, format!("{:<15}{}", r.name, r.description)))
                .collect(),
            Answer::Sync => vec![Item::new("yes", "Now"), Item::new("no", "Later")],
            _ => Vec::new(),
        }
    }

    /// The value of `answer` as shown: the label of a list's item.
    pub fn shown(&self, answer: Answer) -> String {
        if answer == Answer::Repositories {
            let chosen = self.chosen(answer);
            return if chosen.is_empty() {
                "none".into()
            } else {
                chosen.join(", ")
            };
        }
        let value = self.value(answer);
        self.items(answer)
            .into_iter()
            .find(|i| i.value == value)
            .map_or_else(|| value.to_owned(), |i| i.label)
    }

    pub fn fields(&self) -> Vec<Field> {
        let field = |answer, kind| Field { answer, kind };
        let text = |answer| field(answer, Kind::Text);
        let radio = |answer| field(answer, Kind::Radio(self.items(answer)));
        match self.screen() {
            Screen::Welcome | Screen::Summary => Vec::new(),
            Screen::Keyboard => vec![
                field(Answer::Keymap, Kind::Picker),
                text(Answer::KeyboardTest),
            ],
            Screen::Region => vec![
                field(Answer::Locale, Kind::Picker),
                field(Answer::Timezone, Kind::Picker),
            ],
            Screen::Network => {
                let mut fields = vec![radio(Answer::Network)];
                if self.value(Answer::Network) == "static" {
                    fields.push(text(Answer::Address));
                    fields.push(text(Answer::Gateway));
                }
                fields.push(text(Answer::Dns));
                fields.push(radio(Answer::Dnssec));
                fields
            }
            Screen::Storage => vec![
                field(Answer::Disk, Kind::Disks(self.items(Answer::Disk))),
                radio(Answer::Filesystem),
                text(Answer::Swap),
            ],
            Screen::Identity => vec![text(Answer::Hostname)],
            Screen::Access => vec![
                text(Answer::Admin),
                field(Answer::Password, Kind::Secret),
                field(Answer::PasswordAgain, Kind::Secret),
                radio(Answer::Administration),
            ]
            .into_iter()
            .chain(
                self.root_password_asked()
                    .then(|| {
                        [
                            field(Answer::RootPassword, Kind::Secret),
                            field(Answer::RootPasswordAgain, Kind::Secret),
                        ]
                    })
                    .into_iter()
                    .flatten(),
            )
            .chain([radio(Answer::Ssh)])
            // Keys only open SSH: asked for when it is enabled, GitHub's
            // when they can be fetched.
            .chain(self.github_reachable().then(|| text(Answer::Github)))
            .chain(self.ssh_enabled().then(|| text(Answer::PublicKey)))
            .collect(),
            // Adding a repository clones it.
            Screen::Software if self.offline() => vec![text(Answer::Packages)],
            Screen::Software => vec![
                field(Answer::Repositories, Kind::Choices),
                radio(Answer::Sync),
                text(Answer::Packages),
            ],
        }
    }

    /// Everything the focus goes through, in order: fields, then the
    /// buttons that can be pressed.
    pub fn controls(&self) -> Vec<Control> {
        let mut controls: Vec<Control> = self.fields().into_iter().map(Control::Field).collect();
        controls.extend(
            self.buttons()
                .into_iter()
                .filter(|(_, enabled)| *enabled)
                .map(|(b, _)| Control::Button(b)),
        );
        controls
    }

    pub fn focused(&self) -> Option<Control> {
        self.controls().into_iter().nth(self.focus)
    }

    /// The main button's control index: the last one.
    fn primary(&self) -> usize {
        self.controls().len().saturating_sub(1)
    }

    fn set_focus(&mut self, index: usize) {
        self.focus = index;
        self.cursor = match self.focused() {
            Some(Control::Field(f)) => self.value(f.answer).chars().count(),
            _ => 0,
        };
    }

    fn move_focus(&mut self, forward: bool) {
        let count = self.controls().len().max(1);
        let next = if forward {
            (self.focus + 1) % count
        } else {
            (self.focus + count - 1) % count
        };
        self.set_focus(next);
    }

    /// After a field: the next field, or the main button after the last one.
    fn hand_on(&mut self) {
        match self.controls().get(self.focus + 1) {
            Some(Control::Field(_)) => self.set_focus(self.focus + 1),
            _ => self.set_focus(self.primary()),
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> Event {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Event::Quit;
        }
        if let Some(modal) = self.modal.take() {
            return self.handle_modal(modal, key.code, ctrl);
        }
        let Some(control) = self.focused() else {
            return Event::Nothing;
        };
        match (key.code, &control) {
            (KeyCode::Esc, _) => self.back(),

            (KeyCode::PageDown, _) if self.screen() == Screen::Summary => self.scroll += 5,
            (KeyCode::PageUp, _) => self.scroll = self.scroll.saturating_sub(5),
            (KeyCode::Up | KeyCode::Down, Control::Field(f))
                if matches!(f.kind, Kind::Disks(_)) =>
            {
                self.choose_nearby(f, key.code == KeyCode::Down);
            }
            (KeyCode::BackTab | KeyCode::Up, _) => self.move_focus(false),
            (KeyCode::Tab | KeyCode::Down, _) => self.move_focus(true),
            (KeyCode::Enter | KeyCode::Char(' '), Control::Button(button)) => {
                return self.press(*button);
            }
            (KeyCode::Left | KeyCode::Right, Control::Button(_)) => {
                let first_button = self.fields().len();
                if key.code == KeyCode::Left && self.focus > first_button {
                    self.set_focus(self.focus - 1);
                } else if key.code == KeyCode::Right && self.focus < self.primary() {
                    self.set_focus(self.focus + 1);
                }
            }
            (KeyCode::Enter | KeyCode::Char(' '), Control::Field(f))
                if matches!(f.kind, Kind::Picker | Kind::Choices) =>
            {
                self.open_picker(f, String::new());
            }
            (KeyCode::Char(c), Control::Field(f))
                if matches!(f.kind, Kind::Picker | Kind::Choices) && !ctrl =>
            {
                self.open_picker(f, c.to_string());
            }
            (KeyCode::Left | KeyCode::Right | KeyCode::Char(' '), Control::Field(f))
                if matches!(f.kind, Kind::Radio(_)) =>
            {
                self.choose_nearby(f, key.code != KeyCode::Left);
            }
            (KeyCode::Enter, Control::Field(_)) => self.hand_on(),
            (_, Control::Field(f)) if matches!(f.kind, Kind::Text | Kind::Secret) => {
                self.edit(f.answer, key.code, ctrl);
            }
            _ => {}
        }
        Event::Nothing
    }

    /// Typed text that arrived at once (a paste).
    pub fn paste(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        match self.modal.clone() {
            Some(Modal::Picker {
                answer,
                query,
                chosen,
                ..
            }) => {
                let query = query + &text;
                let cursor = self.line_of_value(answer, &query);
                self.modal = Some(Modal::Picker {
                    answer,
                    query,
                    cursor,
                    chosen,
                });
            }
            Some(Modal::Confirm { .. } | Modal::Offline { .. }) => {}
            None => {
                if let Some(Control::Field(f)) = self.focused()
                    && matches!(f.kind, Kind::Text | Kind::Secret)
                {
                    let mut chars: Vec<char> = self.value(f.answer).chars().collect();
                    let at = self.cursor.min(chars.len());
                    let count = text.chars().count();
                    chars.splice(at..at, text.chars());
                    self.set(f.answer, chars.into_iter().collect::<String>());
                    self.cursor = at + count;
                }
            }
        }
    }

    /// Move a radio's or a list's value to the next or previous item.
    fn choose_nearby(&mut self, field: &Field, forward: bool) {
        let (Kind::Radio(items) | Kind::Disks(items)) = &field.kind else {
            return;
        };
        if items.is_empty() {
            return;
        }
        let current = items
            .iter()
            .position(|i| i.value == self.value(field.answer))
            .unwrap_or(0);
        let next = match (&field.kind, forward) {
            // A radio goes round; a list stops at its ends.
            (Kind::Radio(_), true) => (current + 1) % items.len(),
            (Kind::Radio(_), false) => (current + items.len() - 1) % items.len(),
            (_, true) => (current + 1).min(items.len() - 1),
            (_, false) => current.saturating_sub(1),
        };
        self.set(field.answer, items[next].value.clone());
    }

    fn edit(&mut self, answer: Answer, code: KeyCode, ctrl: bool) {
        let mut chars: Vec<char> = self.value(answer).chars().collect();
        let at = self.cursor.min(chars.len());
        let before = chars.clone();
        match code {
            KeyCode::Char('u') if ctrl => {
                chars.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !ctrl && !c.is_control() => {
                chars.insert(at, c);
                self.cursor = at + 1;
            }
            KeyCode::Backspace if at > 0 => {
                chars.remove(at - 1);
                self.cursor = at - 1;
            }
            KeyCode::Delete if at < chars.len() => {
                chars.remove(at);
            }
            KeyCode::Left => self.cursor = at.saturating_sub(1),
            KeyCode::Right => self.cursor = (at + 1).min(chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = chars.len(),
            _ => {}
        }
        if chars != before {
            self.set(answer, chars.into_iter().collect::<String>());
            // Its confirmation is to be checked again.
            if let Some((_, again)) = PASSWORDS.iter().find(|(first, _)| *first == answer) {
                self.issues.retain(|i| i.answer != Some(*again));
            }
        }
    }

    /// Items of `answer` matching `query`, case aside.
    pub fn matches(&self, answer: Answer, query: &str) -> Vec<Item> {
        let query = query.to_lowercase();
        self.items(answer)
            .into_iter()
            .filter(|i| i.label.to_lowercase().contains(&query))
            .collect()
    }

    /// The current value's line among the matches, or the first.
    fn line_of_value(&self, answer: Answer, query: &str) -> usize {
        let value = self.value(answer);
        self.matches(answer, query)
            .iter()
            .position(|i| i.value == value)
            .unwrap_or(0)
    }

    fn open_picker(&mut self, field: &Field, query: String) {
        let answer = field.answer;
        let cursor = self.line_of_value(answer, &query);
        let chosen = matches!(field.kind, Kind::Choices).then(|| self.chosen(answer));
        self.modal = Some(Modal::Picker {
            answer,
            query,
            cursor,
            chosen,
        });
    }

    fn handle_modal(&mut self, modal: Modal, code: KeyCode, ctrl: bool) -> Event {
        match modal {
            Modal::Picker {
                answer,
                query,
                cursor,
                chosen,
            } => self.handle_picker(answer, query, cursor, chosen, code, ctrl),
            Modal::Offline { reason, proceed } => {
                match code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                        self.modal = Some(Modal::Offline {
                            reason,
                            proceed: !proceed,
                        });
                    }
                    KeyCode::Enter | KeyCode::Char(' ') if proceed => {
                        self.online = Some(false);
                        self.next_screen();
                    }
                    // Back to the settings, from the first.
                    KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Esc => self.set_focus(0),
                    _ => self.modal = Some(Modal::Offline { reason, proceed }),
                }
                Event::Nothing
            }
            Modal::Confirm { install } => match code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                    self.modal = Some(Modal::Confirm { install: !install });
                    Event::Nothing
                }
                KeyCode::Enter | KeyCode::Char(' ') if install => Event::Install,
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Esc => {
                    self.scroll = 0;
                    Event::Nothing
                }
                _ => {
                    match code {
                        KeyCode::PageDown => self.scroll += 5,
                        KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
                        _ => {}
                    }
                    self.modal = Some(Modal::Confirm { install });
                    Event::Nothing
                }
            },
        }
    }

    /// A key in an open list: `query` searches it, `cursor` is the
    /// highlighted line, `chosen` the choices made, when several can be.
    fn handle_picker(
        &mut self,
        answer: Answer,
        mut query: String,
        mut cursor: usize,
        mut chosen: Option<Vec<String>>,
        code: KeyCode,
        ctrl: bool,
    ) -> Event {
        let count = self.matches(answer, &query).len();
        let last = count.saturating_sub(1);
        match code {
            KeyCode::Esc => return Event::Nothing,
            KeyCode::Enter if chosen.is_some() => {
                let chosen = chosen.unwrap_or_default();
                let value: Vec<String> = self
                    .items(answer)
                    .into_iter()
                    .filter(|i| chosen.contains(&i.value))
                    .map(|i| i.value)
                    .collect();
                self.set(answer, value.join(" "));
                self.hand_on();
                return Event::Nothing;
            }
            KeyCode::Char('a') if ctrl && chosen.is_some() => {
                let all: Vec<String> = self.items(answer).into_iter().map(|i| i.value).collect();
                let taken = chosen.get_or_insert_default();
                if all.iter().all(|v| taken.contains(v)) {
                    taken.clear();
                } else {
                    *taken = all;
                }
            }
            KeyCode::Char(' ') if chosen.is_some() => {
                if let (Some(item), Some(list)) =
                    (self.matches(answer, &query).get(cursor), chosen.as_mut())
                {
                    match list.iter().position(|v| *v == item.value) {
                        Some(at) => drop(list.remove(at)),
                        None => list.push(item.value.clone()),
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(item) = self.matches(answer, &query).get(cursor) {
                    let value = item.value.clone();
                    self.set(answer, value.clone());
                    self.hand_on();
                    return if answer == Answer::Keymap {
                        Event::Keymap(value)
                    } else {
                        Event::Nothing
                    };
                }
            }
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(last),
            KeyCode::PageUp => cursor = cursor.saturating_sub(10),
            KeyCode::PageDown => cursor = (cursor + 10).min(last),
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = last,
            KeyCode::Char('u') if ctrl => {
                query.clear();
                cursor = self.line_of_value(answer, &query);
            }
            KeyCode::Backspace => {
                query.pop();
                cursor = self.line_of_value(answer, &query);
            }
            KeyCode::Char(c) if !ctrl && !c.is_control() => {
                query.push(c);
                cursor = self.line_of_value(answer, &query);
            }
            _ => {}
        }
        self.modal = Some(Modal::Picker {
            answer,
            query,
            cursor,
            chosen,
        });
        Event::Nothing
    }

    fn press(&mut self, button: Button) -> Event {
        match button {
            Button::Shell => return Event::Quit,
            Button::Back => self.back(),
            Button::Start | Button::Continue => return self.advance(),
            Button::Install => {
                self.issues = self.check(Screen::Summary);
                if self.issues.is_empty() {
                    self.scroll = 0;
                    self.modal = Some(Modal::Confirm { install: false });
                }
            }
        }
        Event::Nothing
    }

    fn back(&mut self) {
        if self.screen > 0 {
            self.screen -= 1;
            self.issues.clear();
            self.scroll = 0;
            self.set_focus(self.primary());
        }
    }

    fn advance(&mut self) -> Event {
        self.issues = self.check(self.screen());
        if let Some(answer) = self.issues.iter().find_map(|i| i.answer) {
            let index = self
                .controls()
                .iter()
                .position(|c| matches!(c, Control::Field(f) if f.answer == answer));
            if let Some(index) = index {
                self.set_focus(index);
            }
            return Event::Nothing;
        }
        if !self.issues.is_empty() || self.screen + 1 >= Screen::ALL.len() {
            return Event::Nothing;
        }
        if self.screen() == Screen::Network {
            self.checking = true;
            return Event::CheckNetwork;
        }
        self.next_screen();
        Event::Nothing
    }

    /// Whether the network settings reached the Internet: on to the next
    /// screen, or asked whether to go on without it.
    pub fn checked(&mut self, result: std::result::Result<(), String>) {
        self.checking = false;
        match result {
            Ok(()) => {
                self.online = Some(true);
                self.next_screen();
            }
            Err(reason) => {
                self.modal = Some(Modal::Offline {
                    reason,
                    proceed: false,
                });
            }
        }
    }

    /// The network settings cannot be checked now, for `why`.
    pub fn cancel_check(&mut self, why: String) {
        self.checking = false;
        self.warn(why);
    }

    fn next_screen(&mut self) {
        self.screen += 1;
        self.enter_screen();
    }

    /// Focus on the first field, or on the main button of a screen without.
    fn enter_screen(&mut self) {
        self.scroll = 0;
        let first = if self.fields().is_empty() {
            self.primary()
        } else {
            0
        };
        self.set_focus(first);
    }

    /// Back to the summary after a failed installation: everything must be
    /// confirmed again.
    pub fn back_to_summary(&mut self) {
        self.screen = Screen::ALL.len() - 1;
        self.modal = None;
        self.issues.clear();
        self.enter_screen();
    }

    /// Show `message` on the current screen.
    pub fn warn(&mut self, message: String) {
        self.issues.push(Issue {
            answer: None,
            message,
        });
    }

    /// Take the machine's disks as they are now. `false`, and back to the
    /// storage screen, when the chosen one is gone or is another disk under
    /// the same name (a disk that reports no serial number is only known by
    /// its name, model and size).
    pub fn recheck_disks(&mut self, disks: Vec<Disk>) -> bool {
        let chosen = self.value(Answer::Disk).to_owned();
        let before = self
            .catalog
            .disks
            .iter()
            .find(|d| d.path == chosen)
            .cloned();
        let same = before.is_some() && disks.iter().find(|d| d.path == chosen) == before.as_ref();
        self.catalog.disks = disks;
        if !same {
            self.screen = Screen::ALL
                .iter()
                .position(|s| *s == Screen::Storage)
                .expect("a storage screen");
            self.modal = None;
            self.set_focus(0);
            self.issues = vec![Issue::on(
                Answer::Disk,
                format!(
                    "{chosen} changed since it was chosen (a disk was added or removed): choose the disk again"
                ),
            )];
        }
        same
    }

    /// What is wrong on `screen`; on the summary, anywhere.
    fn check(&self, screen: Screen) -> Vec<Issue> {
        let config = self.draft();
        let everywhere = screen == Screen::Summary;
        let here = |s: Screen| everywhere || s == screen;
        let admin_empty = self.value(Answer::Admin).trim().is_empty();
        let mut issues: Vec<Issue> = config
            .problems()
            .into_iter()
            .filter(|p| here(Screen::asking(&p.key)))
            .filter(|p| !(admin_empty && p.key == "admin.name"))
            // Said below, about the field to fill.
            .filter(|p| p.key != "admin.sudo")
            .map(|p| match p.key.as_str() {
                "admin" => Issue {
                    answer: None,
                    message: if self.ssh_enabled() {
                        "Nobody could log in: give a password, an SSH key or a GitHub user."
                    } else {
                        "Nobody could log in: give a password, or enable SSH for keys."
                    }
                    .into(),
                },
                key => Issue {
                    answer: Answer::for_key(key),
                    message: p.message,
                },
            })
            .collect();
        if here(Screen::Software) && !issues.iter().any(|i| i.answer == Some(Answer::Packages)) {
            for spec in &config.packages.world {
                if let Err(e) = self
                    .catalog
                    .repositories
                    .resolve(spec, &config.repositories.enable)
                {
                    issues.push(Issue::on(Answer::Packages, e));
                }
            }
        }
        if here(Screen::Access) {
            if admin_empty {
                issues.push(Issue::on(Answer::Admin, "enter a user name"));
            }
            if self.root_password_asked() && self.root_password().is_none() {
                issues.push(Issue::on(
                    Answer::RootPassword,
                    "enter a password for root, or choose sudo",
                ));
            }
            for (first, again) in PASSWORDS {
                if first == Answer::RootPassword && !self.root_password_asked() {
                    continue;
                }
                if self.value(first) != self.value(again) {
                    issues.push(Issue::on(again, "the two passwords differ"));
                }
                if self.value(first).len() > MAX_PASSWORD {
                    issues.push(Issue::on(
                        first,
                        format!("too long: {MAX_PASSWORD} bytes at most"),
                    ));
                }
            }
        }
        let storage_fine = !issues
            .iter()
            .any(|i| matches!(i.answer, Some(Answer::Disk | Answer::Swap)));
        if here(Screen::Storage) && storage_fine {
            match self
                .catalog
                .disks
                .iter()
                .find(|d| d.path == config.storage.disk)
            {
                None => issues.push(Issue::on(Answer::Disk, "no disk to install on")),
                Some(disk) => {
                    let needed = Plan::new(&config.storage, &disk.path).min_disk_size();
                    if disk.size < needed {
                        issues.push(Issue::on(
                            Answer::Disk,
                            format!("too small: this layout needs at least {}", human(needed)),
                        ));
                    }
                }
            }
        }
        issues
    }

    /// The configuration so far, stand-ins for the passwords' hashes.
    fn draft(&self) -> Config {
        let draft = |password: Option<&str>| password.map(|_| Secret::new(DRAFT_HASH));
        self.build(
            draft(self.password(Answer::Password)),
            draft(self.root_password()),
        )
    }

    /// The configuration to install, the passwords hashed by `hash`.
    pub fn config(&self, hash: impl Fn(&str) -> Result<String>) -> Result<Config> {
        let hashed = |password: Option<&str>| -> Result<Option<Secret>> {
            Ok(password.map(&hash).transpose()?.map(Secret::new))
        };
        Ok(self.build(
            hashed(self.password(Answer::Password))?,
            hashed(self.root_password())?,
        ))
    }

    fn build(&self, password_hash: Option<Secret>, root_hash: Option<Secret>) -> Config {
        let value = |answer| self.value(answer).trim().to_owned();
        let optional = |answer| Some(value(answer)).filter(|v| !v.is_empty());
        let mut config = Config::default();
        config.system.keymap = value(Answer::Keymap);
        config.system.locale = value(Answer::Locale);
        config.system.timezone = value(Answer::Timezone);
        config.system.hostname = value(Answer::Hostname);
        if value(Answer::Network) == "static" {
            config.network.mode = NetworkMode::Static;
            config.network.address = optional(Answer::Address);
            config.network.gateway = optional(Answer::Gateway);
        }
        config.network.dns = value(Answer::Dns)
            .split([' ', ','])
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        config.network.dnssec = value(Answer::Dnssec);
        config.storage.disk = value(Answer::Disk);
        config.storage.filesystem = match value(Answer::Filesystem).as_str() {
            "ext4" => Filesystem::Ext4,
            "xfs" => Filesystem::Xfs,
            _ => Filesystem::Btrfs,
        };
        config.storage.swap = optional(Answer::Swap).unwrap_or_else(|| "none".into());
        config.root.password_hash = root_hash;
        config.admin = Some(Admin {
            name: value(Answer::Admin),
            sudo: !self.root_password_asked(),
            password_hash,
            authorized_keys: optional(Answer::PublicKey)
                .filter(|_| self.ssh_enabled())
                .into_iter()
                .collect(),
            github: optional(Answer::Github).filter(|_| self.github_reachable()),
        });
        config.ssh.enable = self.ssh_enabled();
        // Bare names resolved: the configuration kept says what was meant.
        config.packages.world = self
            .chosen(Answer::Packages)
            .into_iter()
            .map(|spec| {
                self.catalog
                    .repositories
                    .resolve(&spec, &self.chosen(Answer::Repositories))
                    .unwrap_or(spec)
            })
            .collect();
        if !self.offline() {
            config.repositories.enable = self.chosen(Answer::Repositories);
            config.repositories.sync = value(Answer::Sync) == "yes";
        }
        config
    }

    fn ssh_enabled(&self) -> bool {
        self.value(Answer::Ssh) == "yes"
    }

    /// GitHub keys open SSH, and can be fetched.
    fn github_reachable(&self) -> bool {
        self.ssh_enabled() && !self.offline()
    }

    /// The disk about to be erased, for the confirmation dialog.
    pub fn target(&self) -> String {
        let path = self.value(Answer::Disk);
        self.catalog
            .disks
            .iter()
            .find(|d| d.path == path)
            .map_or_else(|| path.to_owned(), Disk::describe)
    }

    /// How the administrator signs in, in words.
    pub fn access(&self) -> String {
        let mut ways = Vec::new();
        if self.password(Answer::Password).is_some() {
            ways.push("password".to_owned());
        }
        let given = |answer| !self.value(answer).trim().is_empty();
        if self.ssh_enabled() && given(Answer::PublicKey) {
            ways.push("SSH key".to_owned());
        }
        if self.github_reachable() && given(Answer::Github) {
            ways.push(format!(
                "GitHub keys of {}",
                self.value(Answer::Github).trim()
            ));
        }
        ways.join(", ")
    }

    /// The disk to erase, as the machine reported it.
    pub fn disk(&self) -> Option<&Disk> {
        let path = self.value(Answer::Disk);
        self.catalog.disks.iter().find(|d| d.path == path)
    }

    /// The partitions about to be created, once the storage answers hold.
    pub fn plan(&self) -> Option<Plan> {
        let config = self.draft();
        self.check(Screen::Storage)
            .is_empty()
            .then(|| Plan::new(&config.storage, &config.storage.disk))
    }

    /// What is about to be installed, in a line.
    pub fn outline(&self) -> String {
        let swap = match self.value(Answer::Swap).trim() {
            "" | "none" => "no swap".to_owned(),
            size => format!("{size} of swap"),
        };
        let access = self.access();
        let with = if access.is_empty() {
            String::new()
        } else {
            format!(" with {access}")
        };
        format!(
            "{}, {}, {swap}, user {}{with}",
            self.value(Answer::Hostname).trim(),
            self.value(Answer::Filesystem),
            self.value(Answer::Admin).trim()
        )
    }

    /// The summary of every answer, section by section.
    pub fn summary(&self) -> Vec<Row> {
        let config = self.draft();
        let pair = |label, answer| Row::Pair(label, self.shown(answer));
        let mut rows = vec![
            Row::Heading("System"),
            pair("Host name", Answer::Hostname),
            pair("Time zone", Answer::Timezone),
            pair("Keyboard", Answer::Keymap),
            pair("Language", Answer::Locale),
            Row::Heading("Network"),
        ];
        match config.network.mode {
            NetworkMode::Dhcp => rows.push(Row::Pair("Address", "DHCP".into())),
            NetworkMode::Static => {
                rows.push(pair("Address", Answer::Address));
                rows.push(pair("Gateway", Answer::Gateway));
            }
        }
        let dns = if config.network.dns.is_empty() {
            "from DHCP".to_owned()
        } else {
            config.network.dns.join(" ")
        };
        let dnssec = self.shown(Answer::Dnssec).to_lowercase();
        rows.push(Row::Pair("DNS servers", format!("{dns}, DNSSEC {dnssec}")));
        if self.offline() {
            rows.push(Row::Warning(
                "No Internet with these settings: no GitHub keys, no repository to add or sync"
                    .into(),
            ));
        }
        rows.push(Row::Heading("Administrator"));
        rows.push(pair("User name", Answer::Admin));
        rows.push(pair("Admin with", Answer::Administration));
        rows.push(pair("SSH server", Answer::Ssh));
        rows.push(Row::Pair("Signs in with", self.access()));
        rows.push(Row::Heading("Storage"));
        rows.push(pair("Filesystem", Answer::Filesystem));
        rows.push(pair("Swap", Answer::Swap));
        rows.push(Row::Warning(format!("{} will be erased", self.target())));
        rows.push(Row::Heading("Software"));
        let mut enabled = self.catalog.repositories.enabled.clone();
        enabled.extend(config.repositories.enable);
        rows.push(Row::Pair("Repositories", enabled.join(", ")));
        let sync = match (self.offline(), config.repositories.sync) {
            (true, _) => "later: no Internet now",
            (false, true) => "when the installation ends",
            (false, false) => "later, with cave sync",
        };
        rows.push(Row::Pair("Sync", sync.into()));
        let world = if config.packages.world.is_empty() {
            "none".to_owned()
        } else {
            format!(
                "{}, built by your first cave resolve -x world",
                config.packages.world.join(" ")
            )
        };
        rows.push(Row::Pair("Packages", world));
        rows
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::error::Error;
    use crate::repositories::Repository;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIM7w6tvHPjMHe2grkjvMyevSUvyZE/gsp1Xnad41MMU0 ops@example";

    pub(in crate::wizard) fn catalog() -> Catalog {
        let disk = |path: &str, size: u64, serial: &str| Disk {
            path: path.into(),
            size,
            model: "QEMU HARDDISK".into(),
            serial: serial.into(),
            removable: false,
            read_only: false,
            ..Default::default()
        };
        let list = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect();
        Catalog {
            keymaps: list(&["de", "fr", "fr-latin9", "us"]),
            locales: list(&["en_US.UTF-8", "fr_FR.UTF-8"]),
            zones: list(&["UTC", "Europe/Berlin", "Europe/Paris"]),
            disks: vec![
                disk("/dev/sda", 32 << 30, "drive-scsi0"),
                disk("/dev/sdb", 4 << 30, "drive-scsi1"),
            ],
            interfaces: "ens18  UP  192.0.2.50/24".into(),
            uefi: true,
            dnssec: false,
            repositories: Index::of(
                &["arbor", "hardware"],
                [
                    ("arbor", "sys-process/htop"),
                    ("hardware", "sys-apps/pciutils"),
                    ("net", "net-misc/mosh"),
                    ("perl", "dev-perl/htop"),
                    ("rust", "dev-lang/rust"),
                ]
                .map(|(name, package)| Repository {
                    name: name.into(),
                    description: format!("{name} packages"),
                    packages: [package.to_owned()].into(),
                    ..Repository::default()
                })
                .into(),
            ),
            ssh_password: Some("abcd-efgh-jkmn".into()),
        }
    }

    /// Press `keys`; network settings reach the Internet.
    pub(in crate::wizard) fn press(wizard: &mut Wizard, keys: &[KeyCode]) -> Vec<Event> {
        keys.iter()
            .map(|code| {
                let event = wizard.handle(KeyEvent::new(*code, KeyModifiers::NONE));
                if event == Event::CheckNetwork {
                    wizard.checked(Ok(()));
                }
                event
            })
            .collect()
    }

    pub(in crate::wizard) fn typed(text: &str) -> Vec<KeyCode> {
        text.chars().map(KeyCode::Char).collect()
    }

    use KeyCode::{BackTab, Down, Enter, Esc, Left, Right, Tab, Up};

    fn focused_answer(wizard: &Wizard) -> Option<Answer> {
        match wizard.focused() {
            Some(Control::Field(f)) => Some(f.answer),
            _ => None,
        }
    }

    fn focused_button(wizard: &Wizard) -> Option<Button> {
        match wizard.focused() {
            Some(Control::Button(b)) => Some(b),
            _ => None,
        }
    }

    /// On to the administrator, defaults everywhere.
    pub(in crate::wizard) fn to_access(wizard: &mut Wizard) {
        if wizard.screen() == Screen::Welcome {
            // Continue has the focus.
            press(wizard, &[Enter]);
        }
        while wizard.screen() != Screen::Access {
            let screen = wizard.screen();
            // Back from the first field to the main button, then press it.
            press(wizard, &[BackTab, Enter]);
            assert_ne!(wizard.screen(), screen, "{:?}", wizard.issues());
        }
    }

    #[test]
    fn enter_never_skips_a_screen_from_a_field() {
        let mut wizard = Wizard::new(catalog());
        press(&mut wizard, &[Enter]);
        press(&mut wizard, &[Tab]);
        assert_eq!(focused_answer(&wizard), Some(Answer::KeyboardTest));
        // The last field hands the focus to the main button, nothing more.
        press(&mut wizard, &[Enter]);
        assert_eq!(wizard.screen(), Screen::Keyboard);
        assert_eq!(focused_button(&wizard), Some(Button::Continue));
        press(&mut wizard, &[Left]);
        assert_eq!(focused_button(&wizard), Some(Button::Back));
        press(&mut wizard, &[Enter]);
        assert_eq!(wizard.screen(), Screen::Welcome);
    }

    #[test]
    fn a_password_and_defaults_install() {
        let mut wizard = Wizard::new(catalog());
        to_access(&mut wizard);
        assert_eq!(focused_answer(&wizard), Some(Answer::Admin));
        let mut keys = typed("ops");
        keys.push(Enter);
        keys.extend(typed("s3cret"));
        keys.push(Enter);
        keys.extend(typed("s3cret"));
        keys.extend([Enter; 5]);
        press(&mut wizard, &keys);
        assert_eq!(focused_button(&wizard), Some(Button::Continue));
        press(&mut wizard, &[Enter]);
        assert_eq!(wizard.screen(), Screen::Software, "{:?}", wizard.issues());
        press(&mut wizard, &[BackTab, Enter]);
        assert_eq!(wizard.screen(), Screen::Summary, "{:?}", wizard.issues());

        assert_eq!(focused_button(&wizard), Some(Button::Install));
        press(&mut wizard, &[Enter]);
        assert_eq!(wizard.modal(), Some(&Modal::Confirm { install: false }));
        // The dialog starts on Back: Enter alone installs nothing.
        assert_eq!(press(&mut wizard, &[Enter]), [Event::Nothing]);
        assert!(wizard.modal().is_none());
        let events = press(&mut wizard, &[Enter, Right, Enter]);
        assert_eq!(events[2], Event::Install);

        let config = wizard
            .config(|p| {
                assert_eq!(p, "s3cret");
                Ok("$6$real".into())
            })
            .unwrap();
        assert_eq!(config.storage.disk, "/dev/sda");
        let admin = config.admin.unwrap();
        assert_eq!(admin.name, "ops");
        assert_eq!(admin.password_hash.unwrap().expose(), "$6$real");
    }

    #[test]
    fn lists_open_search_and_keep_the_value_on_escape() {
        let mut wizard = Wizard::new(catalog());
        press(&mut wizard, &[Enter, Enter]);
        assert!(matches!(
            wizard.modal(),
            Some(Modal::Picker {
                answer: Answer::Keymap,
                ..
            })
        ));
        let mut keys = typed("latin");
        keys.push(Enter);
        let events = press(&mut wizard, &keys);
        assert_eq!(events.last(), Some(&Event::Keymap("fr-latin9".into())));
        assert_eq!(wizard.value(Answer::Keymap), "fr-latin9");
        assert_eq!(focused_answer(&wizard), Some(Answer::KeyboardTest));

        // Escape, or a search without result, changes nothing.
        press(&mut wizard, &[BackTab, Enter, Down, Esc]);
        assert_eq!(wizard.value(Answer::Keymap), "fr-latin9");
        let mut keys = vec![Enter];
        keys.extend(typed("zzz"));
        keys.push(Enter);
        press(&mut wizard, &keys);
        assert!(
            wizard.modal().is_some(),
            "Enter on no match keeps the list open"
        );
        press(&mut wizard, &[Esc]);
        assert_eq!(wizard.value(Answer::Keymap), "fr-latin9");

        // Typing on a list field opens it with the search started.
        press(&mut wizard, &[BackTab, Enter]);
        assert_eq!(wizard.screen(), Screen::Region);
        press(&mut wizard, &[Tab]);
        let mut keys = typed("paris");
        keys.push(Enter);
        press(&mut wizard, &keys);
        assert_eq!(wizard.value(Answer::Timezone), "Europe/Paris");
    }

    #[test]
    fn a_paste_searches_like_typing() {
        let mut wizard = Wizard::new(catalog());
        press(&mut wizard, &[Enter, Enter]);
        wizard.paste("latin");
        assert!(matches!(
            wizard.modal(),
            Some(Modal::Picker { cursor: 0, .. })
        ));
        assert_eq!(
            press(&mut wizard, &[Enter]),
            [Event::Keymap("fr-latin9".into())]
        );
    }

    #[test]
    fn text_fields_edit_like_text_fields() {
        let mut wizard = Wizard::new(catalog());
        to_access(&mut wizard);
        assert_eq!(focused_answer(&wizard), Some(Answer::Admin));
        let mut keys = typed("opz");
        keys.extend([Left, KeyCode::Backspace]);
        keys.extend(typed("p"));
        keys.extend([KeyCode::Home, KeyCode::Delete, KeyCode::End]);
        keys.extend(typed("s"));
        press(&mut wizard, &keys);
        assert_eq!(wizard.value(Answer::Admin), "pzs");
        wizard.handle(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(wizard.value(Answer::Admin), "");
        wizard.paste("ops\n");
        assert_eq!(wizard.value(Answer::Admin), "ops");
        assert_eq!(wizard.cursor(), 3);
    }

    #[test]
    fn a_static_network_needs_its_addresses() {
        let mut wizard = Wizard::new(catalog());
        press(&mut wizard, &[Enter]);
        press(&mut wizard, &[BackTab, Enter]);
        press(&mut wizard, &[BackTab, Enter]);
        assert_eq!(wizard.screen(), Screen::Network);
        press(&mut wizard, &[Right]);
        assert_eq!(wizard.value(Answer::Network), "static");
        // Continue: the first wrong field takes the focus.
        press(&mut wizard, &[BackTab, Enter]);
        assert_eq!(wizard.screen(), Screen::Network);
        assert_eq!(focused_answer(&wizard), Some(Answer::Address));
        let wrong: Vec<_> = wizard.issues().iter().filter_map(|i| i.answer).collect();
        assert_eq!(wrong, [Answer::Address, Answer::Gateway, Answer::Dns]);

        let mut keys = typed("192.0.2.10/24");
        keys.push(Enter);
        keys.extend(typed("192.0.2.1"));
        keys.push(Enter);
        keys.extend(typed("192.0.2.53"));
        keys.extend([Enter, Enter, Enter]);
        press(&mut wizard, &keys);
        assert_eq!(wizard.screen(), Screen::Storage, "{:?}", wizard.issues());
    }

    #[test]
    fn a_disk_too_small_is_refused_on_its_screen() {
        let mut wizard = Wizard::new(catalog());
        press(&mut wizard, &[Enter]);
        for _ in 0..3 {
            press(&mut wizard, &[BackTab, Enter]);
        }
        assert_eq!(wizard.screen(), Screen::Storage);
        press(&mut wizard, &[Down, Down]);
        assert_eq!(
            wizard.value(Answer::Disk),
            "/dev/sdb",
            "a list stops at its end"
        );
        press(&mut wizard, &[BackTab, Enter]);
        assert_eq!(wizard.screen(), Screen::Storage);
        assert_eq!(wizard.issues()[0].answer, Some(Answer::Disk));
        assert!(wizard.issues()[0].message.contains("too small"));
        press(&mut wizard, &[Up]);
        assert!(
            wizard.issues().is_empty(),
            "changing the disk clears its issue"
        );
    }

    #[test]
    fn nobody_without_a_way_in_and_mismatched_passwords() {
        let mut wizard = Wizard::new(catalog());
        to_access(&mut wizard);
        let mut keys = typed("ops");
        keys.extend([Enter; 8]);
        press(&mut wizard, &keys);
        assert_eq!(wizard.screen(), Screen::Access);
        assert!(
            wizard
                .issues()
                .iter()
                .any(|i| i.answer.is_none() && i.message.contains("Nobody could log in"))
        );

        press(&mut wizard, &[Tab, Tab]);
        assert_eq!(focused_answer(&wizard), Some(Answer::Password));
        let mut keys = typed("one");
        keys.push(Tab);
        keys.extend(typed("two"));
        press(&mut wizard, &keys);
        assert_eq!(wizard.passwords_match(Answer::PasswordAgain), Some(false));
        press(&mut wizard, &[Enter; 6]);
        assert!(
            wizard
                .issues()
                .iter()
                .any(|i| i.answer == Some(Answer::PasswordAgain))
        );
    }

    #[test]
    fn a_key_alone_is_enough_and_needs_no_hash() {
        let mut wizard = Wizard::new(catalog());
        to_access(&mut wizard);
        let mut keys = typed("ops");
        keys.extend([Tab; 6]);
        press(&mut wizard, &keys);
        wizard.paste(KEY);
        press(&mut wizard, &[Enter, Enter, BackTab, Enter]);
        assert_eq!(wizard.screen(), Screen::Summary, "{:?}", wizard.issues());
        let config = wizard.config(|_| panic!("no password to hash")).unwrap();
        assert_eq!(config.admin.unwrap().authorized_keys, [KEY]);
        assert!(wizard.plan().is_some_and(|p| !p.subvolumes.is_empty()));
    }

    #[test]
    fn dnssec_is_preselected_from_the_resolvers() {
        assert_eq!(Wizard::new(catalog()).value(Answer::Dnssec), "no");
        let validating = Catalog {
            dnssec: true,
            ..catalog()
        };
        assert_eq!(Wizard::new(validating).value(Answer::Dnssec), "yes");
    }

    #[test]
    fn root_can_have_a_password_instead_of_sudo() {
        let mut wizard = Wizard::new(catalog());
        to_access(&mut wizard);
        let mut keys = typed("ops");
        keys.push(Tab);
        keys.extend(typed("mine"));
        keys.push(Tab);
        keys.extend(typed("mine"));
        keys.extend([Tab, Right]);
        press(&mut wizard, &keys);
        assert_eq!(wizard.value(Answer::Administration), "root");
        let answers: Vec<Answer> = wizard.fields().iter().map(|f| f.answer).collect();
        assert_eq!(
            answers[3..6],
            [
                Answer::Administration,
                Answer::RootPassword,
                Answer::RootPasswordAgain
            ]
        );
        let flagged = |wizard: &Wizard, answer| {
            wizard
                .check(Screen::Access)
                .iter()
                .any(|i| i.answer == Some(answer))
        };
        assert!(
            flagged(&wizard, Answer::RootPassword),
            "a root password is needed"
        );
        wizard.set(Answer::RootPassword, "toor");
        wizard.set(Answer::RootPasswordAgain, "tool");
        assert!(flagged(&wizard, Answer::RootPasswordAgain));
        assert_eq!(
            wizard.passwords_match(Answer::RootPasswordAgain),
            Some(false)
        );
        wizard.set(Answer::RootPasswordAgain, "toor");
        assert_eq!(wizard.check(Screen::Access), []);
        let config = wizard.config(|p| Ok(format!("$6$hash-of-{p}"))).unwrap();
        let admin = config.admin.unwrap();
        assert!(!admin.sudo);
        assert_eq!(admin.password_hash.unwrap().expose(), "$6$hash-of-mine");
        assert_eq!(
            config.root.password_hash.unwrap().expose(),
            "$6$hash-of-toor"
        );
        // Back to sudo: root's password is dropped, not installed.
        press(&mut wizard, &[Left]);
        let config = wizard.config(|p| Ok(format!("$6$hash-of-{p}"))).unwrap();
        assert!(config.admin.unwrap().sudo);
        assert!(config.root.password_hash.is_none());
    }

    #[test]
    fn keys_are_asked_for_only_with_ssh() {
        let mut wizard = Wizard::new(catalog());
        to_access(&mut wizard);
        let answers =
            |wizard: &Wizard| -> Vec<Answer> { wizard.fields().iter().map(|f| f.answer).collect() };
        assert_eq!(
            answers(&wizard)[4..],
            [Answer::Ssh, Answer::Github, Answer::PublicKey]
        );
        let mut keys = typed("ops");
        keys.extend([Tab; 6]);
        press(&mut wizard, &keys);
        wizard.paste(KEY);
        press(&mut wizard, &[BackTab, BackTab, Right]);
        assert_eq!(wizard.value(Answer::Ssh), "no");
        assert_eq!(answers(&wizard).last(), Some(&Answer::Ssh));
        // The key typed before is not installed, and alone it opens nothing.
        assert_eq!(wizard.access(), "");
        press(&mut wizard, &[Enter, Enter]);
        assert_eq!(wizard.screen(), Screen::Access);
        assert!(
            wizard
                .issues()
                .iter()
                .any(|i| i.message.contains("enable SSH"))
        );
    }

    #[test]
    fn repositories_are_chosen_from_a_list() {
        let mut wizard = Wizard::new(catalog());
        to_access(&mut wizard);
        let mut keys = typed("ops");
        keys.extend([Tab; 6]);
        press(&mut wizard, &keys);
        wizard.paste(KEY);
        press(&mut wizard, &[Enter, Enter]);
        assert_eq!(wizard.screen(), Screen::Software);
        // All of them, to leave out those not wanted.
        assert_eq!(wizard.shown(Answer::Repositories), "net, perl, rust");
        // Ctrl+A takes none of them, then all, then none again.
        let ctrl_a = |wizard: &mut Wizard| {
            wizard.handle(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        };
        press(&mut wizard, &[Enter]);
        ctrl_a(&mut wizard);
        assert!(matches!(
            wizard.modal(),
            Some(Modal::Picker { chosen: Some(chosen), .. }) if chosen.is_empty()
        ));
        press(&mut wizard, &[KeyCode::Char(' ')]);
        ctrl_a(&mut wizard);
        assert!(matches!(
            wizard.modal(),
            Some(Modal::Picker { chosen: Some(chosen), .. }) if chosen.len() == 3
        ));
        ctrl_a(&mut wizard);
        // Space takes or leaves the highlighted line; Enter keeps the choice.
        let mut keys = typed("perl");
        keys.push(KeyCode::Char(' '));
        keys.extend([KeyCode::Backspace; 4]);
        let space = KeyCode::Char(' ');
        keys.extend([space, Down, Down, space, space]);
        press(&mut wizard, &keys);
        assert!(matches!(
            wizard.modal(),
            Some(Modal::Picker { chosen: Some(chosen), .. }) if chosen == &["perl", "net"]
        ));
        press(&mut wizard, &[Enter]);
        assert_eq!(wizard.value(Answer::Repositories), "net perl");
        // Esc drops what changed in the list.
        press(
            &mut wizard,
            &[BackTab, BackTab, Enter, KeyCode::Char(' '), Esc],
        );
        assert_eq!(wizard.shown(Answer::Repositories), "net, perl");
        let config = wizard.config(|_| panic!("no password")).unwrap();
        assert_eq!(config.repositories.enable, ["net", "perl"]);
        assert!(config.repositories.sync);
    }

    #[test]
    fn summary_labels_fit_their_column() {
        let mut wizard = Wizard::new(catalog());
        wizard.set(Answer::Network, "static");
        wizard.set(Answer::Administration, "root");
        // The view gives the right-hand column 14: a space must remain.
        for row in wizard.summary() {
            if let Row::Pair(label, _) = row {
                assert!(label.chars().count() < 14, "{label}");
            }
        }
    }

    #[test]
    fn package_names_are_resolved_or_flagged() {
        let mut wizard = Wizard::new(catalog());
        wizard.set(Answer::Repositories, "");
        wizard.set(Answer::Packages, "htop pciutils");
        assert_eq!(wizard.check(Screen::Software), []);
        let config = wizard.draft();
        assert_eq!(
            config.packages.world,
            ["sys-process/htop", "sys-apps/pciutils"]
        );
        // perl has a dev-perl/htop too.
        wizard.set(Answer::Repositories, "perl");
        wizard.set(Answer::Packages, "htop mosh");
        let issues = wizard.check(Screen::Software);
        let messages: Vec<&str> = issues.iter().map(|i| i.message.as_str()).collect();
        assert!(
            messages[0].contains("ambiguous: dev-perl/htop, sys-process/htop"),
            "{messages:?}"
        );
        assert!(messages[1].contains("\"mosh\" is in none"), "{messages:?}");
        assert!(issues.iter().all(|i| i.answer == Some(Answer::Packages)));
    }

    #[test]
    fn without_internet_github_and_sync_are_left_out() {
        let mut wizard = Wizard::new(catalog());
        press(&mut wizard, &[Enter]);
        press(&mut wizard, &[BackTab, Enter]);
        press(&mut wizard, &[BackTab, Enter]);
        assert_eq!(wizard.screen(), Screen::Network);
        let continued = |wizard: &mut Wizard| {
            wizard.handle(KeyEvent::new(BackTab, KeyModifiers::NONE));
            wizard.handle(KeyEvent::new(Enter, KeyModifiers::NONE))
        };
        assert_eq!(continued(&mut wizard), Event::CheckNetwork);
        assert!(wizard.checking());
        wizard.checked(Err("curl: (6) Could not resolve host: github.com".into()));
        assert!(!wizard.checking());
        assert!(matches!(
            wizard.modal(),
            Some(Modal::Offline { proceed: false, .. })
        ));
        // Review: back on the settings.
        press(&mut wizard, &[Enter]);
        assert_eq!(wizard.screen(), Screen::Network);
        assert!(wizard.modal().is_none());

        assert_eq!(continued(&mut wizard), Event::CheckNetwork);
        wizard.checked(Err("no route".into()));
        press(&mut wizard, &[Right, Enter]);
        assert_eq!(wizard.screen(), Screen::Storage);
        to_access(&mut wizard);
        let answers: Vec<Answer> = wizard.fields().iter().map(|f| f.answer).collect();
        assert!(!answers.contains(&Answer::Github), "{answers:?}");
        assert!(answers.contains(&Answer::PublicKey), "{answers:?}");
        wizard.set(Answer::Github, "octocat");
        let mut keys = typed("ops");
        keys.extend([Tab; 5]);
        press(&mut wizard, &keys);
        wizard.paste(KEY);
        press(&mut wizard, &[Enter, Enter]);
        assert_eq!(wizard.screen(), Screen::Software);
        // Nothing to clone or sync; packages to build later all the same.
        let answers: Vec<Answer> = wizard.fields().iter().map(|f| f.answer).collect();
        assert_eq!(answers, [Answer::Packages]);
        wizard.set(Answer::Repositories, "perl");
        wizard.set(Answer::Packages, "sys-process/htop");
        let config = wizard.config(|_| panic!("no password")).unwrap();
        assert_eq!(config.admin.unwrap().github, None);
        assert_eq!(config.repositories.enable, Vec::<String>::new());
        assert_eq!(config.packages.world, ["sys-process/htop"]);
        assert!(!config.repositories.sync);
        assert!(
            wizard
                .summary()
                .iter()
                .any(|r| matches!(r, Row::Warning(w) if w.contains("No Internet")))
        );
    }

    #[test]
    fn a_machine_that_cannot_be_installed_says_why() {
        let mut catalog = catalog();
        catalog.uefi = false;
        catalog.disks.clear();
        let mut wizard = Wizard::new(catalog);
        assert_eq!(wizard.blockers().len(), 2);
        assert_eq!(
            wizard.buttons(),
            [(Button::Shell, true), (Button::Start, false)]
        );
        assert_eq!(focused_button(&wizard), Some(Button::Shell));
        assert_eq!(press(&mut wizard, &[Enter]), [Event::Quit]);
    }

    #[test]
    fn a_disk_that_changed_name_is_not_erased() {
        let mut wizard = Wizard::new(catalog());
        assert!(wizard.recheck_disks(catalog().disks));
        // Its twin, same model and size, now under the same name.
        let mut disks = catalog().disks;
        disks[0].serial = "serial-of-the-twin".into();
        assert!(!wizard.recheck_disks(disks));
        assert_eq!(wizard.screen(), Screen::Storage);
        assert!(wizard.issues()[0].message.contains("choose the disk again"));
        assert!(!wizard.recheck_disks(Vec::new()));
    }

    #[test]
    fn overlong_passwords_are_refused() {
        let mut wizard = Wizard::new(catalog());
        let long = "x".repeat(MAX_PASSWORD + 1);
        wizard.set(Answer::Password, long.clone());
        wizard.set(Answer::PasswordAgain, long);
        assert!(
            wizard
                .check(Screen::Access)
                .iter()
                .any(|i| i.message.contains("too long"))
        );
    }

    #[test]
    fn the_draft_hash_never_reaches_an_installation() {
        let mut wizard = Wizard::new(catalog());
        wizard.set(Answer::Password, "x");
        assert!(wizard.config(|_| Err(Error::Other("no".into()))).is_err());
        // The stand-in is well formed: validation does not flag it.
        assert!(
            !wizard
                .draft()
                .problems()
                .iter()
                .any(|p| p.key.contains("password_hash"))
        );
    }

    #[test]
    fn every_validation_key_belongs_to_a_screen_and_a_field() {
        // A problem on a key no screen asks for would only show on the
        // summary, with nowhere to fix it.
        let mut config = Config::default();
        config.storage.swap = "lots".into();
        config.system.hostname = "-bad-".into();
        config.system.timezone = "/etc".into();
        config.system.locale = "fr".into();
        config.network.mode = NetworkMode::Static;
        config.repositories.enable = vec!["../etc".into()];
        config.packages.world = vec!["a/b c".into()];
        config.admin = Some(Admin {
            name: "Root!".into(),
            sudo: false,
            password_hash: Some(Secret::new("plain")),
            authorized_keys: vec!["nope".into()],
            github: Some("not a user".into()),
        });
        let problems = config.problems();
        assert!(problems.len() > 8, "{problems:?}");
        for p in problems {
            assert_ne!(Screen::asking(&p.key), Screen::Summary, "{p}");
            assert!(Answer::for_key(&p.key).is_some(), "{p}");
        }
    }
}
