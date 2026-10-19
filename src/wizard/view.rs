//! Drawing the wizard and the installation, after the mockup in
//! `docs/design/tui`. Designed for 80x24 first: from 100 columns the steps
//! show on the left, from 120 a word about the screen on the right. The form
//! scrolls with the focus; the buttons and the keys keep their place at the
//! bottom.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use super::form::{Answer, Button, Control, Field, Kind, Modal, Row, Screen, Wizard};
use super::progress::{Entry, Installation, Phase};
use super::theme::{Theme, Tone};
use crate::install::COMMAND_LOG;
use crate::network;
use crate::storage::{Plan, Role, human};

/// The labels' column, then where controls start.
const LABEL: usize = 18;
const CONTROL: usize = 2 + LABEL;
const FIELD: usize = 44;
const SECRET: usize = 36;
const SIDE: u16 = 24;
const ABOUT: u16 = 24;
/// Disk rows shown at once; the rest scroll.
const DISK_ROWS: usize = 5;

fn sp(n: usize) -> String {
    " ".repeat(n)
}

/// The cells `text` takes on a terminal.
fn columns(text: &str) -> usize {
    Span::raw(text).width()
}

/// The start of `text` that fits in `n` cells.
fn fit(text: &str, n: usize) -> &str {
    let mut used = 0;
    for (i, c) in text.char_indices() {
        used += columns(c.encode_utf8(&mut [0; 4]));
        if used > n {
            return &text[..i];
        }
    }
    text
}

/// `text` padded or cut to `n` cells.
fn pad(text: &str, n: usize) -> String {
    let text = fit(text, n);
    format!("{text}{}", sp(n - columns(text)))
}

/// `text` cut into lines of at most `width` characters, at spaces.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let needed = columns(&line) + usize::from(!line.is_empty()) + columns(word);
            if !line.is_empty() && needed > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
            while columns(&line) > width {
                // At least a character, should it be wider than a line.
                let head = match fit(&line, width) {
                    "" => line.chars().next().map_or("", |c| &line[..c.len_utf8()]),
                    head => head,
                };
                let rest = line[head.len()..].to_owned();
                lines.push(head.to_owned());
                line = rest;
            }
        }
        lines.push(line);
    }
    lines
}

/// Where things go on the screen.
struct Areas {
    header: Rect,
    bar: Rect,
    side: Option<Rect>,
    body: Rect,
    about: Option<Rect>,
    buttons: Rect,
    keys: Rect,
}

/// The layout; `about`: whether a wide screen keeps room for the About panel.
fn areas(area: Rect, about: bool) -> Areas {
    let row = |y: u16| Rect {
        y: area.y + y.min(area.height.saturating_sub(1)),
        height: 1,
        ..area
    };
    let mut body = Rect {
        x: area.x + 2,
        y: area.y + 3,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(7),
    };
    let side = (area.width >= 100).then(|| {
        let side = Rect {
            x: area.x,
            width: SIDE,
            ..body
        };
        body.x += SIDE;
        body.width = body.width.saturating_sub(SIDE);
        side
    });
    let about = (about && area.width >= 120).then(|| {
        let about = Rect {
            x: body.right().saturating_sub(ABOUT),
            width: ABOUT,
            ..body
        };
        body.width = body.width.saturating_sub(ABOUT);
        about
    });
    Areas {
        header: row(0),
        bar: row(1),
        side,
        body,
        about,
        buttons: row(area.height.saturating_sub(3)),
        keys: row(area.height.saturating_sub(1)),
    }
}

/// `left` and `right` on one line of `width`.
fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(|s| width_of(s)).sum();
    let mut spans = left;
    spans.push(Span::raw(sp(usize::from(width).saturating_sub(used))));
    spans.extend(right);
    Line::from(spans)
}

fn width_of(span: &Span) -> usize {
    columns(&span.content)
}

fn header(frame: &mut Frame, theme: Theme, area: Rect, step: &str, name: &str) {
    let left = vec![
        Span::raw("  "),
        Span::styled("DELI", theme.fg(Tone::Accent)),
        Span::styled("   Exherbo Linux installer", theme.fg(Tone::Secondary)),
    ];
    let right = vec![
        Span::styled(step.to_owned(), theme.fg(Tone::Secondary)),
        Span::styled(format!("   {name}  "), theme.fg(Tone::Text)),
    ];
    frame.render_widget(spread(left, right, area.width), area);
}

/// A thin bar, `done` of `total` in `tone`, the rest in the surface tone.
fn bar(theme: Theme, width: usize, done: usize, total: usize, tone: Tone) -> Vec<Span<'static>> {
    let filled = (width * done / total.max(1)).min(width);
    let (on, off) = theme.bar();
    vec![
        Span::styled(on.repeat(filled), theme.fg(tone)),
        Span::styled(off.repeat(width - filled), theme.fg(Tone::Surface)),
    ]
}

fn progress(frame: &mut Frame, theme: Theme, area: Rect, done: usize, total: usize, tone: Tone) {
    let mut spans = vec![Span::raw("  ")];
    spans.extend(bar(
        theme,
        usize::from(area.width).saturating_sub(4),
        done,
        total,
        tone,
    ));
    frame.render_widget(Line::from(spans), area);
}

/// A button as drawn: its text, what it does, and whether it has the focus.
struct ButtonView {
    label: String,
    kind: ButtonKind,
    enabled: bool,
    focused: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonKind {
    Plain,
    Primary,
    Danger,
}

fn buttons(frame: &mut Frame, theme: Theme, area: Rect, list: &[ButtonView]) {
    let mut spans = Vec::new();
    for button in list {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let text = if !button.enabled {
            format!(" {}: unavailable ", button.label)
        } else if theme.plain {
            format!("[ {} ]", button.label)
        } else {
            format!(" {} ", button.label)
        };
        let style = match (button.enabled, button.focused, button.kind) {
            (false, ..) => theme.fg(Tone::Secondary),
            (true, true, ButtonKind::Danger) => theme.filled(Tone::Error),
            (true, true, _) => theme.focus(),
            (true, false, ButtonKind::Danger) => theme.on_surface(Tone::Error),
            (true, false, ButtonKind::Primary) => theme.on_surface(Tone::Accent),
            (true, false, ButtonKind::Plain) => theme.on_surface(Tone::Secondary),
        };
        spans.push(Span::styled(text, style));
    }
    spans.push(Span::raw("  "));
    frame.render_widget(Line::from(spans).right_aligned(), area);
}

/// The keys' legend, as many as fit, most useful first.
fn keys(frame: &mut Frame, theme: Theme, area: Rect, list: &[(&str, &str)]) {
    let mut spans = vec![Span::raw("  ")];
    let mut used = 2;
    for (key, what) in list {
        let (key, what) = if theme.plain {
            (format!("[{key}]"), format!(" {what}   "))
        } else {
            (format!(" {key} "), format!(" {what}    "))
        };
        used += columns(&key) + columns(&what);
        if used > usize::from(area.width) {
            break;
        }
        spans.push(Span::styled(key, theme.on_surface(Tone::Text)));
        spans.push(Span::styled(what, theme.fg(Tone::Secondary)));
    }
    frame.render_widget(Line::from(spans), area);
}

/// The keys every screen offers, after its own.
const ALWAYS: [(&str, &str); 3] = [
    ("ctrl+t", "plain"),
    ("ctrl+l", "redraw"),
    ("alt+f2", "shell"),
];

fn form_keys(theme: Theme, wizard: &Wizard) -> Vec<(&'static str, &'static str)> {
    let (up_down, left_right) = theme.arrows();
    let mut list = match (wizard.modal(), wizard.focused()) {
        (Some(Modal::Picker { chosen: None, .. }), _) => {
            return vec![
                ("type", "search"),
                (up_down, "move"),
                ("enter", "choose"),
                ("esc", "cancel"),
            ];
        }
        (Some(Modal::Picker { .. }), _) => {
            return vec![
                ("space", "add/remove"),
                ("ctrl+a", "all/none"),
                ("enter", "done"),
                ("type", "search"),
                (up_down, "move"),
                ("esc", "cancel"),
            ];
        }
        (Some(Modal::Offline { .. }), _) => {
            return vec![
                (left_right, "choose"),
                ("enter", "confirm"),
                ("esc", "back"),
            ];
        }
        (Some(Modal::Confirm { .. }), _) => {
            return vec![
                (left_right, "choose"),
                ("enter", "confirm"),
                ("esc", "back"),
                ("pgdn", "more"),
            ];
        }
        (None, Some(Control::Field(field))) => match field.kind {
            Kind::Text | Kind::Secret => {
                vec![("tab", "move"), ("ctrl+u", "clear"), ("esc", "back")]
            }
            Kind::Picker | Kind::Choices => {
                vec![("enter", "open list"), ("tab", "move"), ("esc", "back")]
            }
            Kind::Radio(_) => vec![("tab", "move"), (left_right, "choose"), ("esc", "back")],
            Kind::Disks(_) => vec![(up_down, "choose disk"), ("tab", "move"), ("esc", "back")],
        },
        _ => vec![("tab", "move"), ("enter", "select"), ("esc", "back")],
    };
    if wizard.screen() == Screen::Summary {
        list.push(("pgdn", "more"));
    }
    list.extend(ALWAYS);
    list
}

/// The steps, on the left of a wide console.
fn steps(frame: &mut Frame, theme: Theme, area: Rect, wizard: &Wizard, installing: bool) {
    let mut lines = Vec::new();
    let mut step = |mark: &str, title: &str, mark_tone: Tone, tone: Tone| {
        lines.push(Line::from(vec![
            Span::styled(format!("  {mark:<2}"), theme.fg(mark_tone)),
            Span::styled(title.to_owned(), theme.fg(tone)),
        ]));
    };
    for screen in Screen::ALL {
        if !installing && screen == wizard.screen() {
            step(theme.pointer(), screen.title(), Tone::Accent, Tone::Accent);
        } else if installing || wizard.done(screen) {
            step(
                theme.check(),
                screen.title(),
                Tone::Success,
                Tone::Secondary,
            );
        } else {
            step("", screen.title(), Tone::Secondary, Tone::Secondary);
        }
    }
    if installing {
        step(theme.pointer(), "Install", Tone::Accent, Tone::Accent);
    } else {
        step("", "Install", Tone::Secondary, Tone::Secondary);
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// More about the screen, on the right of a very wide console.
fn about(frame: &mut Frame, theme: Theme, area: Rect, text: &str) {
    let room = usize::from(area.width).saturating_sub(4);
    let mut lines = vec![
        Line::from(""),
        Line::styled("  ABOUT THIS STEP", theme.on_surface(Tone::Secondary)),
        Line::from(""),
    ];
    lines.extend(
        wrap(text, room)
            .into_iter()
            .map(|l| Line::styled(format!("  {l}"), theme.on_surface(Tone::Text))),
    );
    lines.push(Line::from(""));
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .min(area.height);
    let area = Rect { height, ..area };
    frame.render_widget(Block::new().style(theme.on_surface(Tone::Text)), area);
    frame.render_widget(Paragraph::new(lines), area);
}

pub fn draw(frame: &mut Frame, wizard: &Wizard, theme: Theme) {
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.ground()), area);
    let parts = areas(area, true);
    let (step, steps_count) = wizard.step();
    let name = match wizard.modal() {
        Some(Modal::Confirm { .. }) => "Confirm",
        Some(Modal::Offline { .. }) => "No Internet",
        _ => wizard.screen().title(),
    };
    header(
        frame,
        theme,
        parts.header,
        &format!("{step} / {steps_count}"),
        name,
    );
    progress(frame, theme, parts.bar, step, steps_count, Tone::Accent);
    if let Some(side) = parts.side {
        steps(frame, theme, side, wizard, false);
    }
    if let Some(area) = parts.about {
        about(frame, theme, area, wizard.screen().about());
    }

    let width = usize::from(parts.body.width);
    let (rows, start) = match (wizard.modal(), wizard.screen()) {
        (Some(Modal::Confirm { .. }), _) => (confirm(wizard, theme, width), wizard.scroll()),
        (Some(Modal::Offline { reason, .. }), _) => (offline(theme, width, reason), 0),
        (_, Screen::Welcome) => (welcome(wizard, theme, width), 0),
        (_, Screen::Summary) => (summary(wizard, theme, width), wizard.scroll()),
        _ => {
            // Blank lines between fields, as many as fit.
            let tight = form(wizard, theme, width, 0).len();
            let gaps = usize::from(parts.body.height).saturating_sub(tight);
            (form(wizard, theme, width, gaps), 0)
        }
    };
    let cursor = viewport(frame, theme, parts.body, &rows, start);
    buttons(frame, theme, parts.buttons, &form_buttons(wizard, theme));
    if wizard.checking() {
        let text = format!(
            "  Checking the connection with these settings{}",
            theme.ellipsis()
        );
        frame.render_widget(Line::styled(text, theme.fg(Tone::Accent)), parts.keys);
    } else {
        keys(frame, theme, parts.keys, &form_keys(theme, wizard));
    }
    match wizard.modal() {
        Some(Modal::Picker {
            answer,
            query,
            cursor,
            chosen,
        }) => {
            let list = Choosing {
                answer: *answer,
                query,
                cursor: *cursor,
                chosen: chosen.as_deref(),
            };
            picker(frame, theme, parts.body, wizard, &list);
        }
        Some(Modal::Confirm { .. } | Modal::Offline { .. }) => {}
        None => {
            if let Some(position) = cursor {
                frame.set_cursor_position(position);
            }
        }
    }
}

fn form_buttons(wizard: &Wizard, theme: Theme) -> Vec<ButtonView> {
    let pair = |back: &str, go: &str, kind, go_focused: bool| {
        vec![
            ButtonView {
                label: back.into(),
                kind: ButtonKind::Plain,
                enabled: true,
                focused: !go_focused,
            },
            ButtonView {
                label: go.into(),
                kind,
                enabled: true,
                focused: go_focused,
            },
        ]
    };
    match wizard.modal() {
        Some(Modal::Confirm { install }) => {
            return pair("Back", "Erase and install", ButtonKind::Danger, *install);
        }
        Some(Modal::Offline { proceed, .. }) => {
            return pair(
                "Review settings",
                "Continue offline",
                ButtonKind::Plain,
                *proceed,
            );
        }
        _ => {}
    }
    let focused = match wizard.focused() {
        Some(Control::Button(button)) if wizard.modal().is_none() => Some(button),
        _ => None,
    };
    wizard
        .buttons()
        .into_iter()
        .map(|(button, enabled)| {
            let (label, kind) = match button {
                Button::Start | Button::Continue => (
                    format!("{}  {}", button.label(), theme.arrow()),
                    ButtonKind::Primary,
                ),
                Button::Install => (
                    format!("{}{}", button.label(), theme.ellipsis()),
                    ButtonKind::Primary,
                ),
                Button::Back | Button::Shell => (button.label().to_owned(), ButtonKind::Plain),
            };
            ButtonView {
                label,
                kind,
                enabled,
                focused: Some(button) == focused,
            }
        })
        .collect()
}

/// A line of the scrolling body: whether it belongs to the focused control,
/// whether it must show, and the text cursor's column on it.
struct BodyRow {
    line: Line<'static>,
    focused: bool,
    anchor: bool,
    cursor: Option<u16>,
}

impl BodyRow {
    fn plain(line: Line<'static>) -> Self {
        Self {
            line,
            focused: false,
            anchor: false,
            cursor: None,
        }
    }

    fn blank() -> Self {
        Self::plain(Line::from(""))
    }
}

/// The screen's heading and introduction.
fn heading(theme: Theme, width: usize, title: Line<'static>, intro: &str) -> Vec<BodyRow> {
    let mut rows = vec![BodyRow::plain(title)];
    for line in wrap(intro, width.saturating_sub(2)) {
        rows.push(BodyRow::plain(Line::styled(
            format!("  {line}"),
            theme.fg(Tone::Secondary),
        )));
    }
    rows.push(BodyRow::blank());
    rows
}

fn titled(theme: Theme, screen: Screen, width: usize) -> Vec<BodyRow> {
    let title = Line::styled(format!("  {}", screen.heading()), theme.fg(Tone::Text));
    heading(theme, width, title, screen.intro())
}

/// A section's name, in capitals.
fn section(theme: Theme, name: &str) -> BodyRow {
    BodyRow::plain(Line::styled(
        format!("  {}", name.to_uppercase()),
        theme.fg(Tone::Secondary),
    ))
}

/// Draw `rows` in `area`, scrolled so the anchor and as much as fits of the
/// focused control show, or from `start`; the text cursor's position.
fn viewport(
    frame: &mut Frame,
    theme: Theme,
    area: Rect,
    rows: &[BodyRow],
    start: usize,
) -> Option<Position> {
    let height = usize::from(area.height);
    let mut top = start.min(rows.len().saturating_sub(height));
    if let Some(anchor) = rows.iter().position(|r| r.anchor) {
        top = top.min(anchor).max((anchor + 1).saturating_sub(height));
        if let (Some(first), Some(last)) = (
            rows.iter().position(|r| r.focused),
            rows.iter().rposition(|r| r.focused),
        ) {
            if last >= top + height && last + 1 - height <= anchor {
                top = last + 1 - height;
            }
            if first < top && first + height > anchor {
                top = first;
            }
        }
    }
    let mut cursor = None;
    for (i, row) in rows.iter().skip(top).take(height).enumerate() {
        let y = area.y + u16::try_from(i).unwrap_or(u16::MAX);
        frame.render_widget(
            row.line.clone(),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
        if let Some(x) = row.cursor {
            cursor = Some(Position::new(area.x + x, y));
        }
    }
    let mut marker = |text: String, y: u16| {
        let width = u16::try_from(columns(&text)).unwrap_or(0).min(area.width);
        let at = Rect {
            x: area.right() - width,
            y,
            width,
            height: 1,
        };
        frame.render_widget(Span::styled(text, theme.fg(Tone::Secondary)), at);
    };
    if top > 0 {
        marker(format!("{} more", theme.up()), area.y);
    }
    if top + height < rows.len() {
        marker(
            format!("{} more", theme.down()),
            area.bottom().saturating_sub(1),
        );
    }
    cursor
}

fn welcome(wizard: &Wizard, theme: Theme, width: usize) -> Vec<BodyRow> {
    let mut rows = titled(theme, Screen::Welcome, width);
    let mut push = |line: Line<'static>| rows.push(BodyRow::plain(line));
    push(section(theme, "This machine").line);
    let check = |ok: bool, what: &str, value: String| {
        let (mark, tone) = if ok {
            (theme.check(), Tone::Success)
        } else {
            (theme.cross(), Tone::Error)
        };
        Line::from(vec![
            Span::styled(format!("  {mark} "), theme.fg(tone)),
            Span::styled(pad(what, 12), theme.fg(Tone::Secondary)),
            Span::styled(value, theme.fg(if ok { Tone::Text } else { Tone::Error })),
        ])
    };
    let firmware = if wizard.uefi() {
        "UEFI"
    } else {
        "legacy BIOS: UEFI is needed"
    };
    push(check(wizard.uefi(), "Firmware", firmware.into()));
    if wizard.disks().is_empty() {
        push(check(
            false,
            "Disk",
            "none besides the installation medium".into(),
        ));
    }
    for disk in wizard.disks() {
        let mut value = format!("{}   {}   {}", disk.path, human(disk.size), disk.model);
        if !disk.serial.is_empty() {
            value = format!("{value}   {}", disk.serial);
        }
        push(check(true, "Disk", value));
    }
    let address = first_address(wizard);
    let network = interfaces(wizard).into_iter().next();
    push(check(
        network.is_some(),
        "Network",
        network.unwrap_or_else(|| "no address yet".into()),
    ));
    if let Some(password) = wizard.ssh_password() {
        let at = address.map_or_else(
            || "root, once the network has an address".to_owned(),
            |a| format!("root@{a}"),
        );
        push(check(true, "SSH", format!("{at}   password {password}")));
    }
    push(Line::from(""));
    let blockers = wizard.blockers();
    if blockers.is_empty() {
        push(section(theme, "Next").line);
        let next: Vec<&str> = Screen::ALL[1..].iter().map(|s| s.title()).collect();
        for line in wrap(
            &next.join(&format!(" {} ", theme.dot())),
            width.saturating_sub(2),
        ) {
            push(Line::styled(format!("  {line}"), theme.fg(Tone::Text)));
        }
        push(Line::from(""));
        let note = "Nothing is written to a disk until you confirm on the summary.";
        for line in wrap(note, width.saturating_sub(2)) {
            push(Line::styled(format!("  {line}"), theme.fg(Tone::Secondary)));
        }
    }
    for blocker in blockers {
        for line in wrap(&format!("Error: {blocker}"), width.saturating_sub(2)) {
            push(Line::styled(format!("  {line}"), theme.fg(Tone::Error)));
        }
    }
    rows
}

/// This live session's first address, without its prefix.
fn first_address(wizard: &Wizard) -> Option<String> {
    wizard.interfaces().lines().find_map(|line| {
        line.split_whitespace()
            .find_map(|w| w.split_once('/'))
            .map(|(address, _)| address.to_owned())
    })
}

/// This live session's interfaces: name and IPv4 addresses.
fn interfaces(wizard: &Wizard) -> Vec<String> {
    wizard
        .interfaces()
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let name = words.next()?;
            let addresses: Vec<&str> = words.filter(|w| w.contains('/')).collect();
            (!addresses.is_empty()).then(|| format!("{name}   {}", addresses.join("  ")))
        })
        .collect()
}

/// A label and its value, wrapped in its own column.
fn labelled(
    theme: Theme,
    label: &str,
    value: &str,
    label_width: usize,
    width: usize,
) -> Vec<BodyRow> {
    wrap(value, width.saturating_sub(2 + label_width))
        .into_iter()
        .enumerate()
        .map(|(n, line)| {
            let label = if n == 0 { label } else { "" };
            BodyRow::plain(Line::from(vec![
                Span::styled(
                    format!("  {}", pad(label, label_width)),
                    theme.fg(Tone::Secondary),
                ),
                Span::styled(line, theme.fg(Tone::Text)),
            ]))
        })
        .collect()
}

fn summary(wizard: &Wizard, theme: Theme, width: usize) -> Vec<BodyRow> {
    let mut rows = titled(theme, Screen::Summary, width);
    let summary = wizard.summary();
    let mut i = 0;
    while i < summary.len() {
        match &summary[i] {
            Row::Heading(name) => {
                if i > 0 {
                    rows.push(BodyRow::blank());
                }
                rows.push(section(theme, name));
            }
            Row::Pair(label, value) => {
                // Two pairs on a line when both are short.
                let next = match summary.get(i + 1) {
                    Some(Row::Pair(l, v))
                        if columns(value) <= 18 && 2 + 16 + 20 + 14 + columns(v) <= width =>
                    {
                        Some((*l, v.clone()))
                    }
                    _ => None,
                };
                match next {
                    Some((next_label, next_value)) => {
                        rows.push(BodyRow::plain(Line::from(vec![
                            Span::styled(
                                format!("  {}", pad(label, 16)),
                                theme.fg(Tone::Secondary),
                            ),
                            Span::styled(pad(value, 20), theme.fg(Tone::Text)),
                            Span::styled(pad(next_label, 14), theme.fg(Tone::Secondary)),
                            Span::styled(next_value, theme.fg(Tone::Text)),
                        ])));
                        i += 1;
                    }
                    None => rows.extend(labelled(theme, label, value, 16, width)),
                }
            }
            Row::Warning(text) => {
                for (n, line) in wrap(text, width.saturating_sub(4)).into_iter().enumerate() {
                    let mark = if n == 0 { "! " } else { "  " };
                    rows.push(BodyRow::plain(Line::styled(
                        format!("  {mark}{line}"),
                        theme.fg(Tone::Warning),
                    )));
                }
            }
        }
        i += 1;
    }
    rows
}

/// A line of a panel on the surface, filled to `width`, blank ones too.
fn panel(theme: Theme, mut spans: Vec<Span<'static>>, width: usize) -> BodyRow {
    let used: usize = spans.iter().map(width_of).sum();
    spans.push(Span::styled(
        sp(width.saturating_sub(used)),
        theme.on_surface(Tone::Text),
    ));
    BodyRow::plain(Line::from(spans))
}

/// The erase confirmation: a screen of its own, nothing else to press.
fn confirm(wizard: &Wizard, theme: Theme, width: usize) -> Vec<BodyRow> {
    let title = Line::from(vec![
        Span::styled("  ! ", theme.fg(Tone::Warning)),
        Span::styled(
            "Erase this disk and install Exherbo Linux?",
            theme.fg(Tone::Warning),
        ),
    ]);
    let mut rows = heading(theme, width, title, "Everything on it will be lost.");
    // The panel's text, wrapped in its margins.
    let room = width.saturating_sub(4);
    let text = |rows: &mut Vec<BodyRow>, text: &str, tone: Tone| {
        if text.is_empty() {
            return;
        }
        for line in wrap(text, room) {
            let line = Span::styled(format!("  {line}"), theme.on_surface(tone));
            rows.push(panel(theme, vec![line], width));
        }
    };
    let blank = |rows: &mut Vec<BodyRow>| rows.push(panel(theme, vec![], width));
    blank(&mut rows);
    match wizard.disk() {
        Some(disk) => {
            text(&mut rows, &disk.path, Tone::Accent);
            text(
                &mut rows,
                &format!("{}   {}", human(disk.size), disk.model),
                Tone::Text,
            );
            text(&mut rows, &disk.identity(), Tone::Secondary);
            blank(&mut rows);
            text(
                &mut rows,
                &format!("Now on it   {}", disk.contents()),
                Tone::Text,
            );
        }
        None => text(&mut rows, &wizard.target(), Tone::Accent),
    }
    blank(&mut rows);
    rows.push(BodyRow::blank());
    let then = format!(
        "Then installed: {}. It takes a few minutes and cannot be interrupted safely.",
        wizard.outline()
    );
    for line in wrap(&then, width.saturating_sub(2)) {
        rows.push(BodyRow::plain(Line::styled(
            format!("  {line}"),
            theme.fg(Tone::Secondary),
        )));
    }
    rows
}

/// The network settings reach no Internet: what that leaves out, and
/// whether to go on.
fn offline(theme: Theme, width: usize, reason: &str) -> Vec<BodyRow> {
    let title = Line::from(vec![
        Span::styled("  ! ", theme.fg(Tone::Warning)),
        Span::styled(
            "No Internet connection with these settings",
            theme.fg(Tone::Warning),
        ),
    ]);
    let tried = format!(
        "This session tried {} and {}.",
        network::EXHERBO_GIT,
        network::GITHUB
    );
    let mut rows = heading(theme, width, title, &tried);
    let room = width.saturating_sub(4);
    rows.push(panel(theme, vec![], width));
    for line in wrap(reason, room) {
        let line = Span::styled(format!("  {line}"), theme.on_surface(Tone::Text));
        rows.push(panel(theme, vec![line], width));
    }
    rows.push(panel(theme, vec![], width));
    rows.push(BodyRow::blank());
    let then = "Review the settings, or go on without the Internet: the system installs from this ISO all the same, but without GitHub keys, and repositories are added and synced later, with cave.";
    for line in wrap(then, width.saturating_sub(2)) {
        rows.push(BodyRow::plain(Line::styled(
            format!("  {line}"),
            theme.fg(Tone::Secondary),
        )));
    }
    rows
}

/// The fields, a blank line before the first `gaps` after the first.
fn form(wizard: &Wizard, theme: Theme, width: usize, gaps: usize) -> Vec<BodyRow> {
    let mut rows = titled(theme, wizard.screen(), width);
    for (index, field) in wizard.fields().into_iter().enumerate() {
        if (1..=gaps).contains(&index) {
            rows.push(BodyRow::blank());
        }
        let focused = index == wizard.focus() && wizard.modal().is_none();
        rows.extend(control_rows(wizard, theme, &field, focused, width));
        rows.extend(notes(wizard, theme, &field, focused, width));
    }
    rows.extend(extra(wizard, theme, width));
    rows
}

/// A field's label and control.
fn control_rows(
    wizard: &Wizard,
    theme: Theme,
    field: &Field,
    focused: bool,
    width: usize,
) -> Vec<BodyRow> {
    // Secrets are short, which leaves room for their length.
    let size = if matches!(field.kind, Kind::Secret) {
        SECRET
    } else {
        FIELD
    };
    let size = size.min(width.saturating_sub(CONTROL)).max(8);
    let mut line = vec![
        Span::styled(
            if focused {
                format!("{} ", theme.pointer())
            } else {
                "  ".into()
            },
            theme.fg(Tone::Accent),
        ),
        Span::styled(
            pad(field.answer.label(), LABEL),
            theme.fg(if focused {
                Tone::Accent
            } else {
                Tone::Secondary
            }),
        ),
    ];
    let row = |line: Vec<Span<'static>>, cursor: Option<usize>| BodyRow {
        line: Line::from(line),
        focused,
        anchor: focused,
        cursor: cursor.map(|c| u16::try_from(c).unwrap_or(u16::MAX)),
    };
    match &field.kind {
        Kind::Text | Kind::Secret => {
            let (spans, cursor) = input(wizard, theme, field, focused, size);
            line.extend(spans);
            if matches!(field.kind, Kind::Secret)
                && let n @ 1.. = wizard.value(field.answer).chars().count()
                && width >= CONTROL + size + 16
            {
                line.push(Span::styled(
                    format!("  {n} characters"),
                    theme.fg(Tone::Secondary),
                ));
            }
            vec![row(line, cursor.map(|c| CONTROL + c))]
        }
        Kind::Picker | Kind::Choices => {
            let shown = wizard.shown(field.answer);
            let text = format!("{} {}", pad(&shown, size.saturating_sub(6)), theme.down());
            line.extend(field_box(theme, &text, size, focused));
            vec![row(line, None)]
        }
        Kind::Radio(items) => {
            for (n, item) in items.iter().enumerate() {
                if n > 0 {
                    line.push(Span::raw("   "));
                }
                let selected = item.value == wizard.value(field.answer);
                let (mark, mark_style, label_style) = match (selected, focused) {
                    (true, true) if theme.plain => (theme.chosen(), theme.focus(), theme.focus()),
                    (true, true) => (theme.chosen(), theme.fg(Tone::Accent), theme.fg(Tone::Text)),
                    (true, false) => (theme.chosen(), theme.fg(Tone::Text), theme.fg(Tone::Text)),
                    (false, _) => ("( )", theme.fg(Tone::Secondary), theme.fg(Tone::Secondary)),
                };
                line.push(Span::styled(format!("{mark} "), mark_style));
                line.push(Span::styled(item.label.clone(), label_style));
            }
            vec![row(line, None)]
        }
        Kind::Disks(_) => disk_rows(wizard, theme, line, focused, width),
    }
}

/// A box to type in: an accent edge when focused, on the surface; brackets
/// in plain mode.
fn field_box(theme: Theme, text: &str, size: usize, focused: bool) -> Vec<Span<'static>> {
    let inner = size.saturating_sub(4);
    if theme.plain {
        let style = if focused { theme.focus() } else { Style::new() };
        return vec![Span::styled(format!("[ {} ]", pad(text, inner)), style)];
    }
    let edge = if focused { theme.edge() } else { " " };
    vec![
        Span::styled(edge.to_owned(), theme.on_surface(Tone::Accent)),
        Span::styled(
            format!(" {}  ", pad(text, inner)),
            theme.on_surface(Tone::Text),
        ),
    ]
}

/// A text field's box, scrolled to keep the cursor in it; the cursor's
/// column in the box, when focused.
fn input(
    wizard: &Wizard,
    theme: Theme,
    field: &Field,
    focused: bool,
    size: usize,
) -> (Vec<Span<'static>>, Option<usize>) {
    let value: Vec<char> = match field.kind {
        Kind::Secret => {
            std::iter::repeat_n(theme.secret(), wizard.value(field.answer).chars().count())
                .collect()
        }
        _ => wizard.value(field.answer).chars().collect(),
    };
    let inner = size.saturating_sub(4);
    let cursor = if focused {
        wizard.cursor().min(value.len())
    } else {
        value.len()
    };
    // Scrolled so what is before the cursor, and the cursor, fit.
    let mut offset = 0;
    while columns(&value[offset..cursor].iter().collect::<String>()) >= inner.max(1)
        && offset < cursor
    {
        offset += 1;
    }
    let visible: String = value[offset..].iter().collect();
    let before: String = value[offset..cursor].iter().collect();
    (
        field_box(theme, &visible, size, focused),
        focused.then_some(2 + columns(&before)),
    )
}

/// The disks: a table that scrolls around the chosen one, then its whole
/// identity.
fn disk_rows(
    wizard: &Wizard,
    theme: Theme,
    mut label: Vec<Span<'static>>,
    focused: bool,
    width: usize,
) -> Vec<BodyRow> {
    let model_width = width.saturating_sub(CONTROL + 1 + 4 + 14 + 10);
    label.push(Span::styled(
        format!(" {}{}{}MODEL", sp(4), pad("DEVICE", 14), pad("SIZE", 10)),
        theme.fg(Tone::Secondary),
    ));
    let mut rows = vec![BodyRow {
        line: Line::from(label),
        focused,
        anchor: false,
        cursor: None,
    }];
    let disks = wizard.disks();
    let chosen = disks
        .iter()
        .position(|d| d.path == wizard.value(Answer::Disk))
        .unwrap_or(0);
    let first = chosen
        .saturating_sub(DISK_ROWS - 1)
        .min(disks.len().saturating_sub(DISK_ROWS));
    let note = |text: String| BodyRow {
        line: Line::styled(
            format!("{}{text}", sp(CONTROL + 5)),
            theme.fg(Tone::Secondary),
        ),
        focused,
        anchor: false,
        cursor: None,
    };
    if first > 0 {
        rows.push(note(format!("{} {first} more", theme.up())));
    }
    for (i, disk) in disks.iter().enumerate().skip(first).take(DISK_ROWS) {
        let selected = i == chosen;
        let model = if columns(&disk.model) > model_width {
            format!(
                "{}{}",
                pad(&disk.model, model_width.saturating_sub(1)),
                theme.ellipsis()
            )
        } else {
            disk.model.clone()
        };
        let mark = if selected { theme.chosen() } else { "( )" };
        let text = format!(
            "{mark} {}{}{model}",
            pad(&disk.path, 14),
            pad(&human(disk.size), 10)
        );
        let (edge, style) = match (selected, focused) {
            (true, true) => (theme.edge(), theme.focus()),
            (true, false) => (" ", theme.fg(Tone::Text)),
            (false, _) => (" ", theme.fg(Tone::Secondary)),
        };
        rows.push(BodyRow {
            line: Line::from(vec![
                Span::raw(sp(CONTROL)),
                Span::styled(edge.to_owned(), theme.fg(Tone::Accent)),
                Span::styled(pad(&text, width.saturating_sub(CONTROL + 1)), style),
            ]),
            focused,
            anchor: focused && selected,
            cursor: None,
        });
    }
    let after = disks.len().saturating_sub(first + DISK_ROWS);
    if after > 0 {
        rows.push(note(format!("{} {after} more", theme.down())));
    }
    if let Some(disk) = disks.get(chosen) {
        for line in wrap(&disk.identity(), width.saturating_sub(CONTROL + 5)) {
            rows.push(note(line));
        }
    }
    rows
}

/// Under a field: what is wrong with it, whether the passwords match, and
/// its help when it has the focus.
fn notes(
    wizard: &Wizard,
    theme: Theme,
    field: &Field,
    focused: bool,
    width: usize,
) -> Vec<BodyRow> {
    let room = width.saturating_sub(CONTROL + 2);
    let note = |text: &str, tone: Tone, focused: bool| BodyRow {
        line: Line::styled(format!("{}{text}", sp(CONTROL)), theme.fg(tone)),
        focused,
        anchor: false,
        cursor: None,
    };
    let mut rows = Vec::new();
    let mut flagged = false;
    for issue in wizard
        .issues()
        .iter()
        .filter(|i| i.answer == Some(field.answer))
    {
        flagged = true;
        let text = format!("{} Error: {}", theme.cross(), issue.message);
        for line in wrap(&text, room) {
            rows.push(note(&line, Tone::Error, focused));
        }
    }
    if !flagged {
        match wizard.passwords_match(field.answer) {
            Some(true) => rows.push(note(
                &format!("{} The passwords match.", theme.check()),
                Tone::Success,
                focused,
            )),
            Some(false) if !focused => {
                rows.push(note(
                    &format!("{} Error: the two passwords differ.", theme.cross()),
                    Tone::Error,
                    false,
                ));
            }
            _ => {}
        }
    }
    if focused {
        for line in wrap(field.answer.help(wizard.value(Answer::Network)), room) {
            rows.push(note(&line, Tone::Secondary, true));
        }
    }
    rows
}

/// What a screen shows below its fields.
fn extra(wizard: &Wizard, theme: Theme, width: usize) -> Vec<BodyRow> {
    let mut rows = Vec::new();
    match wizard.screen() {
        Screen::Network => {
            let now = interfaces(wizard).join("   ");
            let now = if now.is_empty() {
                "no address yet".to_owned()
            } else {
                now
            };
            rows.push(BodyRow::blank());
            rows.push(BodyRow::plain(Line::from(vec![
                Span::styled("  THIS SESSION NOW   ", theme.fg(Tone::Secondary)),
                Span::styled(now, theme.fg(Tone::Text)),
            ])));
        }
        Screen::Storage => {
            if let Some(plan) = wizard.plan() {
                rows.push(BodyRow::blank());
                rows.extend(partitions(wizard, theme, &plan, width));
            }
        }
        Screen::Software => {
            if !wizard
                .fields()
                .iter()
                .any(|f| f.answer == Answer::Repositories)
            {
                rows.push(BodyRow::blank());
                let note = "Without the Internet, no repository can be added now: each is cloned. On the installed system, cave resolve -x repository/<name> adds one.";
                for line in wrap(note, width.saturating_sub(2)) {
                    rows.push(BodyRow::plain(Line::styled(
                        format!("  {line}"),
                        theme.fg(Tone::Warning),
                    )));
                }
            }
            rows.push(BodyRow::blank());
            rows.push(section(theme, "Repositories always enabled"));
            let enabled = wizard.enabled_repositories().join(", ");
            for line in wrap(&enabled, width.saturating_sub(2)) {
                rows.push(BodyRow::plain(Line::styled(
                    format!("  {line}"),
                    theme.fg(Tone::Text),
                )));
            }
        }
        _ => {}
    }
    rows
}

/// The plan: a schematic bar, then each partition in figures.
fn partitions(wizard: &Wizard, theme: Theme, plan: &Plan, width: usize) -> Vec<BodyRow> {
    let tone = |role: Role| match role {
        Role::Esp => Tone::Warning,
        Role::Swap => Tone::Success,
        Role::Root | Role::Var | Role::Home => Tone::Secondary,
    };
    let mut rows = vec![BodyRow::plain(Line::from(vec![
        Span::styled("  PARTITIONS   ", theme.fg(Tone::Secondary)),
        Span::styled(
            format!("{} will be erased", plan.disk),
            theme.fg(Tone::Warning),
        ),
    ]))];
    let cells = width.saturating_sub(4);
    let disk_size = wizard.disk().map_or(0, |d| d.size).max(1);
    let fixed: Vec<usize> = plan
        .partitions
        .iter()
        .map(|p| {
            p.size.map_or(0, |s| {
                usize::try_from(u128::from(s) * cells as u128 / u128::from(disk_size))
                    .unwrap_or(cells)
                    .max(1)
            })
        })
        .collect();
    let rest = cells.saturating_sub(fixed.iter().sum());
    let (on, _) = theme.bar();
    let mut bar = vec![Span::raw("  ")];
    for (p, cells) in plan.partitions.iter().zip(&fixed) {
        let cells = if p.size.is_none() { rest } else { *cells };
        bar.push(Span::styled(on.repeat(cells), theme.fg(tone(p.role))));
    }
    rows.push(BodyRow::plain(Line::from(bar)));
    let mut line = vec![Span::raw("  ")];
    let mut used = 2;
    for (i, p) in plan.partitions.iter().enumerate() {
        let device = plan.device(i + 1);
        let name = device.rsplit('/').next().unwrap_or(&device);
        let size = p.size.map_or_else(|| "rest".to_owned(), human);
        let mut text = format!(
            "{name}   {size}   {}   {}",
            p.format,
            p.mount.unwrap_or("-")
        );
        if p.role == Role::Root && !plan.subvolumes.is_empty() {
            text = format!("{text}   {} subvolumes", plan.subvolumes.len());
        }
        let item = 2 + columns(&text);
        if used + 6 + item > width && used > 2 {
            rows.push(BodyRow::plain(Line::from(std::mem::replace(
                &mut line,
                vec![Span::raw("  ")],
            ))));
            used = 2;
        } else if used > 2 {
            line.push(Span::raw(sp(6)));
            used += 6;
        }
        line.push(Span::styled(
            format!("{} ", theme.block()),
            theme.fg(tone(p.role)),
        ));
        line.push(Span::styled(text, theme.fg(Tone::Secondary)));
        used += item;
    }
    rows.push(BodyRow::plain(Line::from(line)));
    rows
}

/// An open list: its field, the search, the highlighted line, and for
/// several choices, those made so far.
struct Choosing<'a> {
    answer: Answer,
    query: &'a str,
    cursor: usize,
    chosen: Option<&'a [String]>,
}

/// An open list's search line: the end of a long query stays in sight, the
/// cursor after it.
fn search(frame: &mut Frame, theme: Theme, area: Rect, at: Rect, query: &str) {
    let room = usize::from(area.width).saturating_sub(13);
    let start = query
        .char_indices()
        .rev()
        .take_while(|(i, _)| columns(&query[*i..]) <= room)
        .last()
        .map_or(query.len(), |(i, _)| i);
    let shown = &query[start..];
    frame.render_widget(
        Line::from(vec![
            Span::styled("  Search  ", theme.on_surface(Tone::Secondary)),
            Span::styled(theme.edge().to_owned(), theme.on_surface(Tone::Accent)),
            Span::styled(format!(" {shown}"), theme.on_surface(Tone::Text)),
        ]),
        at,
    );
    let x = area.x + 11 + u16::try_from(columns(shown)).unwrap_or(0);
    frame.set_cursor_position(Position::new(x.min(area.right().saturating_sub(1)), at.y));
}

fn picker(frame: &mut Frame, theme: Theme, body: Rect, wizard: &Wizard, list: &Choosing) {
    let Choosing {
        answer,
        query,
        cursor,
        chosen,
    } = *list;
    // Title, search, then the matches between their "more" lines.
    let width = body.width.saturating_sub(8).min(52);
    let height = body.height.saturating_sub(3).min(14);
    let area = Rect {
        x: body.x + (body.width - width) / 2,
        y: body.y + 3,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme.on_surface(Tone::Text)), area);
    let row = |y: u16| Rect {
        y: area.y + y.min(height.saturating_sub(1)),
        height: 1,
        ..area
    };
    let inner = usize::from(width).saturating_sub(4);
    let line = |text: String, style: Style| {
        Line::from(vec![
            Span::styled("  ", theme.on_surface(Tone::Text)),
            Span::styled(pad(&text, inner), style),
        ])
    };
    let matches = wizard.matches(answer, query);
    let count = if let Some(chosen) = chosen {
        format!("{} chosen  ", chosen.len())
    } else if query.is_empty() {
        format!("{} in all  ", wizard.items(answer).len())
    } else {
        format!("{} matches  ", matches.len())
    };
    frame.render_widget(
        spread(
            vec![Span::styled(
                format!("  {}", answer.list_title()),
                theme.on_surface(Tone::Text),
            )],
            vec![Span::styled(count, theme.on_surface(Tone::Secondary))],
            width,
        ),
        row(0),
    );
    search(frame, theme, area, row(2), query);
    if matches.is_empty() {
        let text = format!("No match for \"{query}\"");
        frame.render_widget(line(text, theme.on_surface(Tone::Error)), row(4));
        return;
    }
    let rows = usize::from(height.saturating_sub(6)).max(1);
    let first = cursor
        .saturating_sub(rows - 1)
        .min(matches.len().saturating_sub(rows));
    let secondary = theme.on_surface(Tone::Secondary);
    if first > 0 {
        let text = format!(" {} {first} more", theme.up());
        frame.render_widget(line(text, secondary), row(3));
    }
    for (i, item) in matches.iter().enumerate().skip(first).take(rows) {
        let y = row(4 + u16::try_from(i - first).unwrap_or(0));
        let label = match chosen {
            Some(chosen) if chosen.contains(&item.value) => {
                format!("{} {}", theme.ticked(), item.label)
            }
            Some(_) => format!("[ ] {}", item.label),
            None => item.label.clone(),
        };
        let shown = if i == cursor {
            line(format!(" {} {label}", theme.pointer()), theme.focus())
        } else {
            line(format!("   {label}"), theme.on_surface(Tone::Text))
        };
        frame.render_widget(shown, y);
    }
    let after = matches.len().saturating_sub(first + rows);
    if after > 0 {
        let text = format!(" {} {after} more", theme.down());
        frame.render_widget(
            line(text, secondary),
            row(4 + u16::try_from(rows).unwrap_or(0)),
        );
    }
}

pub fn draw_install(frame: &mut Frame, run: &Installation, wizard: &Wizard, theme: Theme) {
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.ground()), area);
    let parts = areas(area, false);
    let (index, total, name) = run.step.clone().unwrap_or((0, 1, "starting".into()));
    let (title, done, tone) = match run.phase {
        Phase::Running => ("Installing", 1, Tone::Accent),
        Phase::Done => ("Installed", 1, Tone::Success),
        Phase::Failed(_) => ("Installing", 0, Tone::Error),
    };
    header(frame, theme, parts.header, "", title);
    // Done: the whole bar; failed: up to the step that stopped.
    let (done, total_bar) = if done == 1 { (1, 1) } else { (index, total) };
    progress(frame, theme, parts.bar, done, total_bar, tone);
    if let Some(side) = parts.side {
        steps(frame, theme, side, wizard, true);
    }
    let width = usize::from(parts.body.width);
    let rows = match &run.phase {
        Phase::Running => running(
            run,
            theme,
            width,
            usize::from(parts.body.height),
            (index, total, &name),
        ),
        Phase::Done => installed(run, theme, width),
        Phase::Failed(error) => failed(run, theme, width, error, (index, total, &name)),
    };
    let start = match run.phase {
        Phase::Running => 0,
        _ => run.scroll,
    };
    viewport(frame, theme, parts.body, &rows, start);
    let list: Vec<ButtonView> = run
        .buttons()
        .iter()
        .enumerate()
        .map(|(i, (_, label))| {
            let primary = i + 1 == run.buttons().len();
            ButtonView {
                label: if primary {
                    format!("{label}  {}", theme.arrow())
                } else {
                    (*label).to_owned()
                },
                kind: if primary {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Plain
                },
                enabled: true,
                focused: i == run.button,
            }
        })
        .collect();
    buttons(frame, theme, parts.buttons, &list);
    if run.phase == Phase::Running {
        let line = Line::from(vec![
            Span::styled(
                "  The installation cannot be interrupted safely.    ",
                theme.fg(Tone::Secondary),
            ),
            Span::styled(" alt+f2 ", theme.on_surface(Tone::Text)),
            Span::styled(" shell", theme.fg(Tone::Secondary)),
        ]);
        frame.render_widget(line, parts.keys);
    } else {
        let (_, left_right) = theme.arrows();
        keys(
            frame,
            theme,
            parts.keys,
            &[
                (left_right, "choose"),
                ("enter", "confirm"),
                ("pgdn", "scroll"),
            ],
        );
    }
}

fn clock(run: &Installation) -> String {
    let seconds = run.elapsed().as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn text_rows(text: &str, width: usize, style: Style) -> Vec<BodyRow> {
    wrap(text, width.saturating_sub(2))
        .into_iter()
        .map(|l| BodyRow::plain(Line::styled(format!("  {l}"), style)))
        .collect()
}

/// The running step, a bar of the steps, and the journal's last lines.
fn running(
    run: &Installation,
    theme: Theme,
    width: usize,
    height: usize,
    (index, total, name): (usize, usize, &str),
) -> Vec<BodyRow> {
    const SPINNER: [char; 4] = ['|', '/', '-', '\\'];
    let spin = SPINNER[usize::try_from(run.elapsed().as_millis() / 250).unwrap_or(0) % 4];
    let mut rows = heading(
        theme,
        width,
        Line::styled("  Installing Exherbo Linux", theme.fg(Tone::Text)),
        &format!("on {}", run.target),
    );
    let left = vec![
        Span::styled(format!("  {spin} "), theme.fg(Tone::Accent)),
        Span::styled(name.to_owned(), theme.fg(Tone::Text)),
    ];
    let right = vec![Span::styled(
        format!("step {index} of {total}   {}", clock(run)),
        theme.fg(Tone::Secondary),
    )];
    rows.push(BodyRow::plain(spread(
        left,
        right,
        u16::try_from(width).unwrap_or(u16::MAX),
    )));
    let mut bar_line = vec![Span::raw("  ")];
    bar_line.extend(bar(
        theme,
        width.saturating_sub(2),
        index.saturating_sub(1),
        total,
        Tone::Accent,
    ));
    rows.push(BodyRow::plain(Line::from(bar_line)));
    rows.push(BodyRow::blank());
    rows.push(section(theme, "Journal"));
    let journal: Vec<BodyRow> = run
        .journal
        .iter()
        .map(|entry| match entry {
            Entry::Step { index: i, name } if *i < index => BodyRow::plain(Line::from(vec![
                Span::styled(format!("  {} ", theme.check()), theme.fg(Tone::Success)),
                Span::styled(name.clone(), theme.fg(Tone::Secondary)),
            ])),
            Entry::Step { name, .. } => BodyRow::plain(Line::from(vec![
                Span::styled(format!("  {} ", theme.pointer()), theme.fg(Tone::Accent)),
                Span::styled(name.clone(), theme.fg(Tone::Text)),
            ])),
            Entry::Line(line) => BodyRow::plain(Line::styled(
                format!("    {}", pad(line, width.saturating_sub(4))),
                theme.fg(Tone::Secondary),
            )),
        })
        .collect();
    let room = height.saturating_sub(rows.len());
    let skip = journal.len().saturating_sub(room);
    rows.extend(journal.into_iter().skip(skip));
    rows
}

/// What was installed, and how to reach it.
fn installed(run: &Installation, theme: Theme, width: usize) -> Vec<BodyRow> {
    let title = Line::from(vec![
        Span::styled(format!("  {} ", theme.check()), theme.fg(Tone::Success)),
        Span::styled("Exherbo Linux is installed", theme.fg(Tone::Success)),
    ]);
    let mut rows = heading(
        theme,
        width,
        title,
        &format!("on {} in {}", run.target, clock(run)),
    );
    for (label, value) in &run.facts {
        rows.extend(labelled(theme, label, value, 18, width));
    }
    rows.push(BodyRow::blank());
    rows.extend(text_rows(
        "Remove the installation medium, then reboot into the new system.",
        width,
        theme.fg(Tone::Text),
    ));
    rows.push(BodyRow::blank());
    rows.extend(text_rows(
        "Its configuration is kept in /var/log/deli/deli.toml, password hash removed: with a hash or keys, it installs other servers unattended.",
        width,
        theme.fg(Tone::Secondary),
    ));
    rows
}

/// Where it stopped, whether the disk changed, the error, the journals.
fn failed(
    run: &Installation,
    theme: Theme,
    width: usize,
    error: &str,
    (index, total, name): (usize, usize, &str),
) -> Vec<BodyRow> {
    let title = Line::from(vec![
        Span::styled(format!("  {} ", theme.cross()), theme.fg(Tone::Error)),
        Span::styled("Installation failed", theme.fg(Tone::Error)),
    ]);
    let mut rows = heading(
        theme,
        width,
        title,
        &format!("Stopped at step {index} of {total}, {name}."),
    );
    if run.erasing {
        rows.extend(text_rows(
            "! The disk was being erased: it may no longer hold its data. Installing again erases it.",
            width,
            theme.fg(Tone::Warning),
        ));
    } else {
        rows.extend(text_rows(
            "The disk has not been erased.",
            width,
            theme.fg(Tone::Secondary),
        ));
    }
    rows.push(BodyRow::blank());
    let panel = |text: String| {
        let text = Span::styled(format!("  {text}"), theme.on_surface(Tone::Text));
        panel(theme, vec![text], width)
    };
    rows.push(panel(String::new()));
    for line in wrap(error, width.saturating_sub(4)) {
        rows.push(panel(line));
    }
    rows.push(panel(String::new()));
    rows.push(BodyRow::blank());
    rows.extend(text_rows(
        &format!("The journals are in /run/deli, commands' output in {COMMAND_LOG}. Back to the installer asks again before erasing anything."),
        width,
        theme.fg(Tone::Secondary),
    ));
    rows
}

#[cfg(test)]
pub(super) mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::KeyCode::{
        BackTab, Backspace, Down, Enter, Esc, PageDown, Right, Tab,
    };
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::super::form::tests::{catalog, press, to_access, typed};
    use super::super::theme::Colors;
    use super::*;
    use crate::report::Progress;

    pub fn render(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(draw).unwrap();
        terminal.backend().buffer().clone()
    }

    pub fn text(buffer: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    pub type Shot = Box<dyn Fn(&mut Frame, Theme)>;

    /// Every screen, dialog and installation stage.
    fn snap(wizard: &Wizard) -> Shot {
        let wizard = wizard.clone();
        Box::new(move |f, t| draw(f, &wizard, t))
    }

    pub fn tour() -> Vec<(&'static str, Shot)> {
        let mut shots = Vec::new();
        let mut wizard = Wizard::new(catalog());
        shots.push(("welcome", snap(&wizard)));
        press(&mut wizard, &[Enter]);
        shots.push(("keyboard", snap(&wizard)));
        press(&mut wizard, &[Enter]);
        press(&mut wizard, &typed("fr"));
        shots.push(("keyboard-list", snap(&wizard)));
        press(&mut wizard, &[Esc, BackTab, Enter, BackTab, Enter, Right]);
        shots.push(("network-static", snap(&wizard)));
        press(&mut wizard, &[BackTab, Enter]);
        shots.push(("network-errors", snap(&wizard)));

        let mut wizard = Wizard::new(catalog());
        press(&mut wizard, &[Enter]);
        for _ in 0..3 {
            press(&mut wizard, &[BackTab, Enter]);
        }
        shots.push(("storage", snap(&wizard)));
        press(&mut wizard, &[Tab]);
        shots.push(("storage-filesystem", snap(&wizard)));
        press(&mut wizard, &[BackTab]);
        to_access(&mut wizard);
        let mut keys = typed("ops");
        keys.push(Enter);
        keys.extend(typed("secret"));
        keys.push(Enter);
        keys.extend(typed("secrex"));
        keys.push(Enter);
        press(&mut wizard, &keys);
        shots.push(("administrator", snap(&wizard)));
        let mut root = wizard.clone();
        // From Administration: root gets the password.
        press(&mut root, &[Right, Tab]);
        shots.push(("administrator-root", snap(&root)));
        press(&mut wizard, &[BackTab, Backspace]);
        press(&mut wizard, &typed("t"));
        press(&mut wizard, &[Tab, Tab, Tab, Tab, Enter, Enter]);
        assert_eq!(wizard.screen(), Screen::Software, "{:?}", wizard.issues());
        shots.push(("repositories", snap(&wizard)));
        press(&mut wizard, &[Enter, Down, KeyCode::Char(' ')]);
        shots.push(("repositories-list", snap(&wizard)));
        press(&mut wizard, &[Enter, Enter]);
        press(&mut wizard, &typed("sys-process/htop"));
        shots.push(("software-packages", snap(&wizard)));
        press(&mut wizard, &[Enter, Enter]);
        assert_eq!(wizard.screen(), Screen::Summary, "{:?}", wizard.issues());
        shots.push(("summary", snap(&wizard)));
        press(&mut wizard, &[Enter, Right]);
        shots.push(("confirm", snap(&wizard)));

        shots.extend(connection_shots());

        for phase in ["running", "done", "failed"] {
            let mut run = Installation::new(
                "/dev/sda (32G, QEMU HARDDISK, drive-scsi0)".into(),
                vec![
                    ("Host name", "exherbo".into()),
                    ("User name", "ops".into()),
                    ("Signs in with", "password".into()),
                    ("SSH server", "enabled".into()),
                    ("Address", "from DHCP".into()),
                ],
            );
            let names = [
                "preflight checks",
                "releasing the disk",
                "partitioning",
                "creating filesystems",
                "mounting the target",
                "copying the system",
            ];
            for (i, name) in names.iter().enumerate() {
                run.report(Progress::Step {
                    index: i + 1,
                    total: 13,
                    name: (*name).into(),
                });
                run.report(Progress::Line(format!("      done with {name}")));
            }
            run.report(Progress::Erasing);
            match phase {
                "done" => run.finish(Ok(())),
                "failed" => run.finish(Err("`cp -a /run/rootfsbase/. /run/deli/target/` failed (exit status 1): cp: error writing '/run/deli/target/usr/lib/libLLVM.so': No space left on device".into())),
                _ => {}
            }
            let wizard = wizard.clone();
            let shot: Shot = Box::new(move |f, t| draw_install(f, &run, &wizard, t));
            shots.push((
                match phase {
                    "running" => "install-running",
                    "done" => "install-done",
                    _ => "install-failed",
                },
                shot,
            ));
        }
        shots
    }

    /// The network settings being checked, then found to reach no Internet.
    fn connection_shots() -> Vec<(&'static str, Shot)> {
        let mut wizard = Wizard::new(catalog());
        press(&mut wizard, &[Enter]);
        press(&mut wizard, &[BackTab, Enter, BackTab, Enter]);
        wizard.handle(KeyEvent::new(BackTab, KeyModifiers::NONE));
        wizard.handle(KeyEvent::new(Enter, KeyModifiers::NONE));
        let checking = snap(&wizard);
        wizard.checked(Err(
            "curl: (6) Could not resolve host: gitlab.exherbo.org".into()
        ));
        vec![
            ("network-checking", checking),
            ("network-offline", snap(&wizard)),
        ]
    }

    fn themes() -> [Theme; 4] {
        let plain = Theme::plain(Colors::TrueColor);
        [
            Theme::new(Colors::Console, true),
            Theme::new(Colors::Indexed, false),
            Theme::new(Colors::TrueColor, true),
            plain,
        ]
    }

    #[test]
    fn every_screen_fits_small_and_large_consoles() {
        for (name, shot) in tour() {
            for (w, h) in [
                (80, 24),
                (99, 30),
                (100, 30),
                (119, 36),
                (120, 24),
                (128, 40),
            ] {
                for theme in themes() {
                    let screen = text(&render(w, h, |f| shot(f, theme)));
                    if std::env::var_os("DELI_SHOW").is_some() {
                        println!("== {name} {w}x{h} {theme:?}\n{screen}");
                    }
                    assert!(screen.contains("DELI"), "{name}");
                    // The buttons stay in sight, whatever the size.
                    if !matches!(name, "install-running" | "keyboard-list") {
                        assert!(
                            ["Back", "Live shell", "Review settings"]
                                .iter()
                                .any(|b| screen.contains(b)),
                            "{name} {w}x{h}:\n{screen}"
                        );
                    }
                    // A radio's options are never cut, beside the About panel too.
                    if name.starts_with("network") && name != "network-offline" {
                        assert!(screen.contains("Required"), "{name} {w}x{h}:\n{screen}");
                    }
                    if !theme.rich || theme.plain {
                        assert!(
                            screen.is_ascii(),
                            "{name}: a symbol this terminal may not draw:\n{screen}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_long_confirmation_scrolls_to_its_end() {
        let mut catalog = catalog();
        let disk = &mut catalog.disks[0];
        disk.by_id = Some(format!("scsi-0QEMU_QEMU_HARDDISK_{}", "x".repeat(70)));
        disk.parts = (1..=20)
            .map(|i| crate::system::probe::Part {
                path: format!("/dev/sda{i}"),
                size: 1 << 30,
                fstype: "ext4".into(),
                label: format!("data-{i}"),
            })
            .collect();
        let mut wizard = Wizard::new(catalog);
        to_access(&mut wizard);
        let mut keys = typed("ops");
        keys.push(Tab);
        keys.extend(typed("secret"));
        keys.push(Tab);
        keys.extend(typed("secret"));
        keys.extend([Tab, Tab, Tab, Tab, Enter, Enter, BackTab, Enter, Enter]);
        press(&mut wizard, &keys);
        assert!(matches!(wizard.modal(), Some(Modal::Confirm { .. })));
        let theme = Theme::new(Colors::TrueColor, true);
        let end = "interrupted safely";
        let screen = text(&render(80, 24, |f| draw(f, &wizard, theme)));
        assert!(!screen.contains(end) && screen.contains("more"), "{screen}");
        press(&mut wizard, &[PageDown; 4]);
        let screen = text(&render(80, 24, |f| draw(f, &wizard, theme)));
        assert!(screen.contains(end), "{screen}");
        assert!(matches!(wizard.modal(), Some(Modal::Confirm { .. })));
    }

    #[test]
    fn the_text_cursor_stays_in_its_box() {
        let mut wizard = Wizard::new(catalog());
        press(&mut wizard, &[Enter, Tab]);
        wizard.paste(&"界".repeat(40));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| draw(f, &wizard, Theme::new(Colors::TrueColor, true)))
            .unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        assert!(
            cursor.x < 2 + u16::try_from(CONTROL + FIELD).unwrap(),
            "{cursor:?}"
        );
    }

    #[test]
    fn the_welcome_screen_tells_how_to_ssh_in() {
        let wizard = Wizard::new(catalog());
        let screen = text(&render(80, 24, |f| {
            draw(f, &wizard, Theme::new(Colors::Console, true));
        }));
        assert!(
            screen.contains("root@192.0.2.50   password abcd-efgh-jkmn"),
            "{screen}"
        );
    }

    #[test]
    fn text_is_measured_in_cells() {
        assert_eq!(pad("日本語", 5), "日本 ");
        assert_eq!(wrap("日本語テキスト", 4), ["日本", "語テ", "キス", "ト"]);
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
    }

    #[test]
    fn the_selected_disk_stays_in_sight() {
        let mut catalog = catalog();
        catalog.disks = (0..20)
            .map(|i| crate::system::probe::Disk {
                path: format!("/dev/test{i:02}"),
                size: 64 << 30,
                model: "DISK".into(),
                serial: format!("serial-{i:02}"),
                ..Default::default()
            })
            .collect();
        let mut wizard = Wizard::new(catalog);
        press(&mut wizard, &[Enter]);
        for _ in 0..3 {
            press(&mut wizard, &[BackTab, Enter]);
        }
        assert_eq!(wizard.screen(), Screen::Storage);
        press(&mut wizard, &[Down; 19]);
        let screen = text(&render(80, 24, |f| {
            draw(f, &wizard, Theme::new(Colors::Console, true));
        }));
        assert!(screen.contains("/dev/test19"), "{screen}");
        assert!(screen.contains("serial-19"), "{screen}");
        assert!(screen.contains("15 more"), "{screen}");
    }

    /// The palette, for the preview.
    fn rgb(color: ratatui::style::Color, background: bool) -> String {
        use ratatui::style::Color;
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            Color::Reset if background => "#101418".into(),
            _ => "#edf2f7".into(),
        }
    }

    fn html(buffer: &Buffer) -> String {
        use std::fmt::Write as _;

        use ratatui::style::Modifier;
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                let (mut fg, mut bg) = (rgb(cell.fg, false), rgb(cell.bg, true));
                if cell.modifier.contains(Modifier::REVERSED) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let symbol = cell
                    .symbol()
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                write!(
                    out,
                    "<span style=\"color:{fg};background:{bg}\">{symbol}</span>"
                )
                .unwrap();
            }
            out.push('\n');
        }
        out
    }

    /// `cargo test preview -- --ignored`: every screen, as a true-colour
    /// terminal draws it, in target/wizard-preview.html; each character in a
    /// cell of its own.
    #[test]
    #[ignore = "writes a file, for looking at the screens"]
    fn preview() {
        use std::fmt::Write as _;

        let mut page = String::from(
            "<!doctype html><meta charset=utf-8><style>body{background:#222;color:#ddd;font-family:sans-serif}pre{font:16px/20px monospace;margin:0}pre span{display:inline-block;width:9.6px;text-align:center}</style>",
        );
        for (name, shot) in tour() {
            for (w, h, theme) in [
                (80, 24, Theme::new(Colors::TrueColor, true)),
                (128, 40, Theme::new(Colors::TrueColor, true)),
                (80, 24, Theme::plain(Colors::TrueColor)),
            ] {
                let plain = if theme.plain { " plain" } else { "" };
                write!(page, "<h3>{name} {w}x{h}{plain}</h3><pre>").unwrap();
                page.push_str(&html(&render(w, h, |f| shot(f, theme))));
                page.push_str("</pre>");
            }
        }
        std::fs::write(
            concat!(env!("CARGO_MANIFEST_DIR"), "/target/wizard-preview.html"),
            page,
        )
        .unwrap();
    }
}
