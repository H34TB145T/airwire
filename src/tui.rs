use std::{
    io::{self, stdout},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use tokio::sync::mpsc;

use crate::{
    audio::Audio,
    client::{NetCommand, UiEvent},
};

const BG: Color = Color::Rgb(9, 10, 12);
const PANEL: Color = Color::Rgb(17, 18, 21);
const BORDER: Color = Color::Rgb(55, 58, 64);
const TEXT: Color = Color::Rgb(210, 212, 216);
const MUTED: Color = Color::Rgb(126, 130, 138);
const ACCENT: Color = Color::Rgb(119, 188, 183);
const WARN: Color = Color::Rgb(214, 166, 96);
const ERROR: Color = Color::Rgb(219, 112, 120);

pub struct ViewConfig {
    pub code: String,
    pub relay_display: String,
    pub is_host: bool,
    pub voice_enabled: bool,
    pub startup_message: Option<String>,
}

struct Entry {
    label: String,
    body: String,
    tone: Tone,
}

enum Tone {
    Mine,
    Peer,
    Status,
    Error,
}

struct App {
    config: ViewConfig,
    input: String,
    entries: Vec<Entry>,
    connection: String,
    secure: bool,
    peers: usize,
    should_quit: bool,
    progress: Option<String>,
}

pub async fn run(
    config: ViewConfig,
    network: mpsc::Sender<NetCommand>,
    mut events: mpsc::Receiver<UiEvent>,
) -> Result<()> {
    let mut terminal = TerminalGuard::new()?;
    let mut entries = vec![Entry {
        label: "airwire".into(),
        body: "Type /help for commands. Drag a file into this terminal to send it.".into(),
        tone: Tone::Status,
    }];
    if let Some(invitation) = &config.startup_message {
        entries.push(Entry {
            label: "share".into(),
            body: invitation.clone(),
            tone: Tone::Status,
        });
    }
    let mut app = App {
        config,
        input: String::new(),
        entries,
        connection: "connecting…".into(),
        secure: false,
        peers: 1,
        should_quit: false,
        progress: None,
    };

    let mut audio = if app.config.voice_enabled {
        match Audio::new(network.clone()) {
            Ok(audio) => Some(audio),
            Err(error) => {
                app.push_status(format!("voice unavailable: {error:#}"), true);
                None
            }
        }
    } else {
        None
    };
    let mut input_events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));

    loop {
        terminal.terminal.draw(|frame| draw(frame, &app))?;
        if app.should_quit {
            break;
        }
        tokio::select! {
            _ = tick.tick() => {}
            event = input_events.next() => {
                match event {
                    Some(Ok(event)) => handle_terminal_event(event, &mut app, &network, &mut audio),
                    Some(Err(error)) => return Err(error).context("terminal input failed"),
                    None => break,
                }
            }
            event = events.recv() => {
                let Some(event) = event else {
                    app.push_status("network task stopped".into(), true);
                    continue;
                };
                handle_ui_event(event, &mut app, audio.as_ref());
            }
        }
    }
    if let Some(audio) = audio.as_mut()
        && audio.is_capturing()
    {
        audio.stop_capture();
    }
    let _ = network.try_send(NetCommand::Shutdown);
    Ok(())
}

fn handle_terminal_event(
    event: Event,
    app: &mut App,
    network: &mpsc::Sender<NetCommand>,
    audio: &mut Option<Audio>,
) {
    match event {
        Event::Key(KeyEvent {
            kind: KeyEventKind::Release,
            ..
        }) => {}
        Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        Event::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => app.should_quit = true,
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            ..
        }) => submit_input(app, network, audio),
        Event::Key(KeyEvent {
            code: KeyCode::Backspace,
            ..
        }) => {
            app.input.pop();
        }
        Event::Key(KeyEvent {
            code: KeyCode::Char(character),
            modifiers,
            ..
        }) if !modifiers.contains(KeyModifiers::CONTROL) => app.input.push(character),
        Event::Paste(text) => app.input.push_str(&text),
        _ => {}
    }
}

fn submit_input(app: &mut App, network: &mpsc::Sender<NetCommand>, audio: &mut Option<Audio>) {
    let raw = std::mem::take(&mut app.input);
    let input = raw.trim();
    if input.is_empty() {
        return;
    }
    if input == "/quit" || input == "/exit" {
        app.should_quit = true;
    } else if input == "/help" {
        app.push_status(
            "/send <path> · /call · /hangup · /clear · /quit · drag-and-drop a file".into(),
            false,
        );
    } else if input == "/clear" {
        app.entries.clear();
    } else if input == "/call" {
        match audio {
            Some(audio) => {
                if let Err(error) = audio.start_capture() {
                    app.push_status(format!("could not start voice: {error:#}"), true);
                }
            }
            None => app.push_status(
                "voice is disabled or no audio device is available".into(),
                true,
            ),
        }
    } else if input == "/hangup" {
        if let Some(audio) = audio {
            audio.stop_capture();
        }
    } else if let Some(raw_path) = input.strip_prefix("/send ") {
        send_path(raw_path, app, network);
    } else if input.starts_with('/') {
        app.push_status(format!("unknown command: {input}"), true);
    } else if let Some(path) = dropped_file_path(input) {
        queue_command(app, network, NetCommand::SendFile(path));
    } else {
        queue_command(app, network, NetCommand::Chat(input.to_owned()));
    }
}

fn send_path(raw_path: &str, app: &mut App, network: &mpsc::Sender<NetCommand>) {
    let path = normalize_terminal_path(raw_path);
    if path.is_file() {
        queue_command(app, network, NetCommand::SendFile(path));
    } else {
        app.push_status(format!("file not found: {}", path.display()), true);
    }
}

fn queue_command(app: &mut App, network: &mpsc::Sender<NetCommand>, command: NetCommand) {
    if let Err(error) = network.try_send(command) {
        let message = match error {
            mpsc::error::TrySendError::Full(_) => "outgoing queue is busy; try again",
            mpsc::error::TrySendError::Closed(_) => "network connection is closed",
        };
        app.push_status(message.into(), true);
    }
}

fn dropped_file_path(input: &str) -> Option<PathBuf> {
    let path = normalize_terminal_path(input);
    path.is_file().then_some(path)
}

fn normalize_terminal_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    let normalized = if cfg!(windows) {
        strip_wrapping_quotes(trimmed).to_owned()
    } else {
        unescape_posix_terminal_path(trimmed)
    };
    if normalized.starts_with("file://")
        && let Ok(url) = url::Url::parse(&normalized)
        && let Ok(path) = url.to_file_path()
    {
        return path;
    }
    Path::new(&normalized).to_path_buf()
}

fn strip_wrapping_quotes(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn unescape_posix_terminal_path(value: &str) -> String {
    #[derive(Clone, Copy)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars();
    let mut quote = Quote::None;
    while let Some(character) = characters.next() {
        match (quote, character) {
            (Quote::None, '\'') => quote = Quote::Single,
            (Quote::None, '"') => quote = Quote::Double,
            (Quote::Single, '\'') => quote = Quote::None,
            (Quote::Double, '"') => quote = Quote::None,
            (Quote::None | Quote::Double, '\\') => {
                if let Some(escaped) = characters.next() {
                    output.push(escaped);
                } else {
                    output.push('\\');
                }
            }
            _ => output.push(character),
        }
    }
    output
}

fn handle_ui_event(event: UiEvent, app: &mut App, audio: Option<&Audio>) {
    match event {
        UiEvent::Ready { secure, detail } => {
            app.secure = secure;
            app.connection = detail;
        }
        UiEvent::Chat { alias, text, mine } => app.entries.push(Entry {
            label: alias,
            body: text,
            tone: if mine { Tone::Mine } else { Tone::Peer },
        }),
        UiEvent::Status(message) => app.push_status(message, false),
        UiEvent::Error(message) => app.push_status(message, true),
        UiEvent::PeerCount(count) => app.peers = count,
        UiEvent::FileProgress {
            label,
            transferred,
            total,
        } => {
            let percent = transferred
                .saturating_mul(100)
                .checked_div(total)
                .unwrap_or(100);
            app.progress = Some(format!("{label} · {percent}%"));
            if transferred >= total {
                app.progress = None;
            }
        }
        UiEvent::FileReceived { from, path } => {
            app.progress = None;
            app.push_status(
                format!("file from {from} saved to {}", path.display()),
                false,
            );
        }
        UiEvent::VoiceStarted(alias) => app.push_status(format!("{alias} started voice"), false),
        UiEvent::VoiceStopped(alias) => app.push_status(format!("{alias} stopped voice"), false),
        UiEvent::VoicePcm {
            sample_rate,
            samples,
        } => {
            if let Some(audio) = audio {
                audio.play_pcm(sample_rate, &samples);
            }
        }
        UiEvent::Closed(reason) => {
            app.connection = reason.clone();
            app.secure = false;
            app.push_status(reason, true);
        }
    }
    if app.entries.len() > 2_000 {
        app.entries.drain(..500);
    }
}

impl App {
    fn push_status(&mut self, body: String, error: bool) {
        self.entries.push(Entry {
            label: if error { "error" } else { "system" }.into(),
            body,
            tone: if error { Tone::Error } else { Tone::Status },
        });
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    let security = if app.secure {
        "● encrypted"
    } else {
        "○ negotiating"
    };
    let role = if app.config.is_host { "host" } else { "guest" };
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                " AIRWIRE ",
                Style::default()
                    .fg(BG)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  anonymous rpc chat", Style::default().fg(MUTED)),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" room {} ", app.config.code),
                Style::default().fg(TEXT),
            ),
            Span::styled(
                format!("· {role} · {} peers · ", app.peers),
                Style::default().fg(MUTED),
            ),
            Span::styled(
                security,
                Style::default().fg(if app.secure { ACCENT } else { WARN }),
            ),
        ]),
        Line::from(Span::styled(
            format!(" {}", app.connection),
            Style::default().fg(MUTED),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(PANEL)),
    );
    frame.render_widget(header, sections[0]);

    let available = sections[1].height.saturating_sub(2) as usize;
    let items: Vec<ListItem<'_>> = app
        .entries
        .iter()
        .rev()
        .take(available.max(1))
        .rev()
        .map(|entry| {
            let (label_color, body_color) = match entry.tone {
                Tone::Mine => (ACCENT, TEXT),
                Tone::Peer => (Color::Rgb(166, 154, 201), TEXT),
                Tone::Status => (MUTED, MUTED),
                Tone::Error => (ERROR, ERROR),
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {:>10} ", truncate(&entry.label, 10)),
                    Style::default()
                        .fg(label_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(&entry.body, Style::default().fg(body_color)),
            ]))
        })
        .collect();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" messages ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(BORDER))
                .style(Style::default().bg(BG)),
        ),
        sections[1],
    );

    let input = Paragraph::new(format!("› {}", app.input))
        .style(Style::default().fg(TEXT).bg(PANEL))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(" message or /command ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if app.secure { ACCENT } else { BORDER })),
        );
    frame.render_widget(input, sections[2]);
    let cursor_x = sections[2]
        .x
        .saturating_add(2)
        .saturating_add(app.input.chars().count() as u16)
        .min(sections[2].right().saturating_sub(2));
    frame.set_cursor_position((cursor_x, sections[2].y + 1));

    let footer_text = app.progress.as_deref().unwrap_or(&app.config.relay_display);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" developed by H34TB145T ", Style::default().fg(MUTED)),
            Span::styled("· ", Style::default().fg(BORDER)),
            Span::styled(footer_text, Style::default().fg(MUTED)),
        ]))
        .alignment(Alignment::Center)
        .style(Style::default().bg(BG)),
        sections[3],
    );
}

fn truncate(value: &str, max: usize) -> String {
    let mut characters = value.chars();
    let result: String = characters.by_ref().take(max).collect();
    if characters.next().is_some() && max > 1 {
        format!("{}…", result.chars().take(max - 1).collect::<String>())
    } else {
        result
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode().context("cannot enter raw terminal mode")?;
        let mut output = stdout();
        execute!(output, EnterAlternateScreen, EnableBracketedPaste)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(output))?;
        terminal.clear()?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        App {
            config: ViewConfig {
                code: "aB3xY9".into(),
                relay_display: "test relay".into(),
                is_host: false,
                voice_enabled: false,
                startup_message: None,
            },
            input: String::new(),
            entries: Vec::new(),
            connection: "testing".into(),
            secure: true,
            peers: 2,
            should_quit: false,
            progress: None,
        }
    }

    #[test]
    fn ignores_windows_style_key_release_events() {
        let (network, _receiver) = mpsc::channel(1);
        let mut app = test_app();
        let mut audio = None;

        handle_terminal_event(
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Press,
            )),
            &mut app,
            &network,
            &mut audio,
        );
        handle_terminal_event(
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )),
            &mut app,
            &network,
            &mut audio,
        );

        assert_eq!(app.input, "a");
    }

    #[test]
    fn accepts_quoted_and_escaped_drag_paths() {
        assert_eq!(
            normalize_terminal_path("\"/tmp/a file.txt\""),
            PathBuf::from("/tmp/a file.txt")
        );
        if !cfg!(windows) {
            assert_eq!(
                normalize_terminal_path("/tmp/a\\ file.txt"),
                PathBuf::from("/tmp/a file.txt")
            );
            assert_eq!(
                normalize_terminal_path(
                    "/Users/riki/Downloads/ChatGPT\\ Image\\ Jul\\ 19\\,\\ 2026\\,\\ 06_05_41\\ PM.png"
                ),
                PathBuf::from("/Users/riki/Downloads/ChatGPT Image Jul 19, 2026, 06_05_41 PM.png")
            );
            assert_eq!(
                normalize_terminal_path("/tmp/photo\\ \\#1\\ \\&\\ final.png"),
                PathBuf::from("/tmp/photo #1 & final.png")
            );
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn recognizes_a_real_file_with_macos_drag_escaping() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("Photo Jul 19, 2026, 06_05_41 PM.png");
        std::fs::write(&file, b"photo").unwrap();
        let escaped = file
            .to_string_lossy()
            .replace(' ', "\\ ")
            .replace(',', "\\,");
        assert_eq!(dropped_file_path(&escaped), Some(file));
    }
}
