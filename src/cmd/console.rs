use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use clap::Args;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, widgets::TableState};

use crate::constants::{
    CHANNELS_PER_BANK, MAX_CHANNELS, MAX_LEVEL, NUM_BANKS, POLL_INTERVAL_ACTIVE_MS,
    POLL_INTERVAL_IDLE_MS, POLL_INTERVAL_MS,
};

/// Max attempts to fetch a channel before giving up on it.
const MAX_FETCH_ATTEMPTS: u8 = 3;
use crate::modes::Mode;
use crate::scanner::ScannerClient;
use crate::types::{BankMask, Channel, ChannelIndex, Frequency, Modulation, ScanStatus};

use super::renderer;

#[derive(Args)]
pub struct ConsoleArgs {
    /// Scanner serial device (default: auto-detect the UBC125 by its USB
    /// id, 1965:0018).
    #[arg(short, long, env = "UBC125_DEVICE")]
    pub console_device: Option<String>,
}

#[derive(Default, PartialEq)]
pub(crate) enum InputMode {
    #[default]
    Normal,
    Editing(EditState),
    ConfirmDelete,
    SetLevel(LevelKind),
}

/// Which 0-15 level dialog is active
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum LevelKind {
    Squelch,
    Volume,
}

impl LevelKind {
    pub(crate) fn title(&self) -> &'static str {
        match self {
            LevelKind::Squelch => "Squelch",
            LevelKind::Volume => "Volume",
        }
    }

    fn response_prefix(&self) -> &'static str {
        match self {
            LevelKind::Squelch => "SQL",
            LevelKind::Volume => "VOL",
        }
    }
}

#[derive(Clone, Default, PartialEq)]
pub(crate) enum EditField {
    #[default]
    Frequency,
    Name,
}

#[derive(Clone, Default, PartialEq)]
pub(crate) struct EditState {
    pub(crate) frequency: String,
    pub(crate) name: String,
    pub(crate) active_field: EditField,
}

pub struct App {
    pub(crate) model: String,
    pub(crate) version: String,
    pub(crate) volume: String,
    pub(crate) squelch: String,
    pub(crate) level_input: String,
    pub(crate) scan_status: ScanStatus,
    pub(crate) tabs: Vec<String>,
    pub(crate) selected_tab: usize,
    pub(crate) channels: Vec<Option<Channel>>,
    /// Pending channel fetches as (index, attempts so far).
    pub(crate) fetch_queue: VecDeque<(u32, u8)>,

    /// Cached copy of the scanner mode (synced from the client each loop).
    pub(crate) in_prg_mode: bool,
    pub(crate) banks: BankMask,
    pub(crate) input_mode: InputMode,
    pub(crate) table_state: TableState,
    pub(crate) error: Option<String>,
}

impl App {
    fn new(client: &mut ScannerClient) -> Self {
        let mut tabs = vec!["Monitor".to_string()];
        for i in 1..=NUM_BANKS {
            tabs.push(format!("Bank {}", i));
        }

        let model = client.get_model().unwrap_or_else(|e| format!("Err: {e}"));
        let version = client
            .get_firmware_version()
            .unwrap_or_else(|e| format!("Err: {e}"));
        let volume = client.get_volume().unwrap_or_else(|e| format!("Err: {e}"));
        let squelch = client.get_squelch().unwrap_or_else(|e| format!("Err: {e}"));

        // Fetch initial bank status. Capture any initialization errors for
        // display in the status bar.
        let (banks, init_error) = match client.get_banks() {
            Ok(banks) => (banks, None),
            Err(e) => (
                BankMask::new(),
                Some(format!("Failed to read bank status: {e}")),
            ),
        };

        Self {
            model,
            version,
            volume,
            squelch,
            level_input: String::new(),
            scan_status: ScanStatus::default(),
            tabs,
            selected_tab: 0,
            channels: vec![None; (MAX_CHANNELS + 1) as usize],
            fetch_queue: VecDeque::new(),
            in_prg_mode: false,
            banks,
            input_mode: InputMode::Normal,
            table_state: TableState::default().with_selected(Some(0)),
            error: init_error,
        }
    }

    fn next_tab(&mut self) {
        self.selected_tab = (self.selected_tab + 1) % self.tabs.len();
        self.queue_channels_for_tab();
    }

    fn previous_tab(&mut self) {
        if self.selected_tab > 0 {
            self.selected_tab -= 1;
        } else {
            self.selected_tab = self.tabs.len() - 1;
        }
        self.queue_channels_for_tab();
    }

    fn next_channel(&mut self) {
        let max_row = (CHANNELS_PER_BANK - 1) as usize;
        let i = match self.table_state.selected() {
            Some(i) if i >= max_row => 0,
            Some(i) => i + 1,
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn previous_channel(&mut self) {
        let max_row = (CHANNELS_PER_BANK - 1) as usize;
        let i = match self.table_state.selected() {
            Some(0) => max_row,
            Some(i) => i - 1,
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn queue_channels_for_tab(&mut self) {
        if self.selected_tab == 0 {
            return;
        }
        let bank = self.selected_tab as u32;
        let start_idx = (bank - 1) * CHANNELS_PER_BANK + 1;
        let end_idx = bank * CHANNELS_PER_BANK;

        for i in start_idx..=end_idx {
            let queued = self.fetch_queue.iter().any(|(q, _)| *q == i);
            if self.channels[i as usize].is_none() && !queued {
                self.fetch_queue.push_back((i, 1));
            }
        }
    }

    /// Get the selected channel index (for renderer popup).
    pub(crate) fn selected_channel_index(&self) -> u32 {
        if self.selected_tab == 0 {
            return 0;
        }
        let bank = self.selected_tab as u32;
        let row = self.table_state.selected().unwrap_or(0) as u32;
        (bank - 1) * CHANNELS_PER_BANK + row + 1
    }

    /// Check if in PRG mode (for renderer status bar).
    pub(crate) fn is_in_prg_mode(&self) -> bool {
        self.in_prg_mode
    }
}

pub fn run(args: &ConsoleArgs) -> Result<(), Box<dyn std::error::Error>> {
    let device = crate::detect::resolve_device(args.console_device.as_deref())?;
    let mut client = ScannerClient::new(&device)?;
    let mut app = App::new(&mut client);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut last_poll = Instant::now();

    'main: loop {
        // Mode management: bank tabs need program mode, monitor tab needs
        // monitor mode.
        app.in_prg_mode = client.mode() == Mode::Program;
        if app.selected_tab > 0 && !app.in_prg_mode {
            if let Err(e) = client.ensure_program() {
                app.error = Some(format!("Failed to enter program mode: {e}"));
            }
        } else if app.selected_tab == 0 && app.in_prg_mode {
            match client.ensure_monitor() {
                Ok(()) => {
                    app.fetch_queue.clear();
                }
                Err(e) => {
                    app.error = Some(format!("Failed to return to monitor mode: {e}"));
                }
            }
        }
        app.in_prg_mode = client.mode() == Mode::Program;

        // Fetch Logic
        if app.is_in_prg_mode() {
            if let Some((idx, attempts)) = app.fetch_queue.pop_front() {
                match client.get_channel(idx) {
                    Ok(channel) => app.channels[idx as usize] = Some(channel),
                    Err(e) => {
                        tracing::warn!("fetch channel {idx} failed (attempt {attempts}): {e}");
                        if attempts < MAX_FETCH_ATTEMPTS {
                            app.fetch_queue.push_back((idx, attempts + 1));
                        } else {
                            app.error = Some(format!("Failed to load channel {idx}"));
                        }
                    }
                }
            }
        } else if app.selected_tab == 0
            && last_poll.elapsed() >= Duration::from_millis(POLL_INTERVAL_MS)
        {
            match client.get_status() {
                Ok(status) => app.scan_status = status,
                // Keep the last good status on transient errors.
                Err(e) => tracing::warn!("status poll failed: {e}"),
            }
            last_poll = Instant::now();
        }

        terminal.draw(|f| renderer::render(f, &app))?;

        let poll_timeout = if !app.fetch_queue.is_empty() {
            Duration::from_millis(POLL_INTERVAL_ACTIVE_MS)
        } else {
            Duration::from_millis(POLL_INTERVAL_IDLE_MS)
        };

        if event::poll(poll_timeout)?
            && let Event::Key(key) = event::read()?
        {
            let idx = app.selected_channel_index();
            if handle_input(&mut app, &mut client, key, idx) {
                break 'main;
            }
        }
        // Refresh the cached mode for the next frame (key handlers may have
        // toggled program mode).
        app.in_prg_mode = client.mode() == Mode::Program;
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

// =============================================================================
// Input handlers
// Each returns `true` to signal the app should exit.
// =============================================================================

fn handle_input(app: &mut App, client: &mut ScannerClient, key: KeyEvent, idx: u32) -> bool {
    // Clear any previous error so it can be re-set by the current operation.
    app.error = None;

    match &app.input_mode {
        InputMode::Normal => handle_normal(app, client, key, idx),
        InputMode::ConfirmDelete => handle_confirm_delete(app, client, key, idx),
        InputMode::SetLevel(kind) => handle_set_level(app, client, key, *kind),
        InputMode::Editing(_) => {
            // Extract edit_state to avoid double-borrow of `app`
            let mut edit_state = match std::mem::replace(&mut app.input_mode, InputMode::Normal) {
                InputMode::Editing(s) => s,
                _ => return false,
            };
            let quit = handle_editing(app, client, key, idx, &mut edit_state);
            // std::mem::replace above already set input_mode to Normal.
            // Only restore Editing if the key doesn't close the dialog.
            if !quit && key.code != KeyCode::Esc && key.code != KeyCode::Enter {
                app.input_mode = InputMode::Editing(edit_state);
            }
            quit
        }
    }
}

fn handle_normal(app: &mut App, client: &mut ScannerClient, key: KeyEvent, idx: u32) -> bool {
    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Right => app.next_tab(),
        KeyCode::Left => app.previous_tab(),
        KeyCode::Down | KeyCode::Char('j') if app.selected_tab > 0 => app.next_channel(),
        KeyCode::Up | KeyCode::Char('k') if app.selected_tab > 0 => app.previous_channel(),
        KeyCode::Char('d') if app.selected_tab > 0 => {
            app.input_mode = InputMode::ConfirmDelete;
        }
        KeyCode::Char('e') | KeyCode::Enter if app.selected_tab > 0 => {
            let (freq, name) = if let Some(chan) = &app.channels[idx as usize] {
                (chan.frequency.to_string(), chan.name.clone())
            } else {
                (String::new(), String::new())
            };
            app.input_mode = InputMode::Editing(EditState {
                frequency: freq,
                name,
                active_field: EditField::Frequency,
            });
        }
        KeyCode::Char('s') if app.selected_tab == 0 && client.start_scan().is_err() => {
            app.error = Some("Failed to start scan".to_string());
        }
        KeyCode::Char('l') if app.selected_tab == 0 => {
            app.level_input.clear();
            app.input_mode = InputMode::SetLevel(LevelKind::Squelch);
        }
        KeyCode::Char('v') if app.selected_tab == 0 => {
            app.level_input.clear();
            app.input_mode = InputMode::SetLevel(LevelKind::Volume);
        }
        KeyCode::Char('h') if app.selected_tab == 0 && client.hold_scan().is_err() => {
            app.error = Some("Failed to hold scan".to_string());
        }
        KeyCode::Char(c) if app.selected_tab == 0 && c.is_ascii_digit() => {
            if let Some(digit) = c.to_digit(10) {
                let bank = if digit == 0 { NUM_BANKS as u32 } else { digit };
                let mut new_mask = app.banks.clone();
                new_mask.toggle(bank);
                if client.set_banks(&new_mask).is_ok() {
                    app.banks = new_mask;
                } else {
                    app.error = Some("Failed to update bank mask".to_string());
                }
            }
        }
        _ => {}
    }
    false
}

fn handle_confirm_delete(
    app: &mut App,
    client: &mut ScannerClient,
    key: KeyEvent,
    idx: u32,
) -> bool {
    match key.code {
        KeyCode::Char('y') => {
            match client.delete_channel(idx) {
                Ok(()) => {
                    app.channels[idx as usize] = None;
                    app.fetch_queue.push_back((idx, 1));
                }
                Err(e) => app.error = Some(format!("Failed to delete channel: {e}")),
            }
            app.input_mode = InputMode::Normal;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
        }
        _ => {}
    }
    false
}

fn handle_set_level(
    app: &mut App,
    client: &mut ScannerClient,
    key: KeyEvent,
    kind: LevelKind,
) -> bool {
    match key.code {
        KeyCode::Char(c) if c.is_ascii_digit() && app.level_input.len() < 2 => {
            app.level_input.push(c);
        }
        KeyCode::Backspace => {
            app.level_input.pop();
        }
        KeyCode::Enter => {
            if let Ok(lvl) = app.level_input.parse::<u8>()
                && lvl <= MAX_LEVEL
            {
                let ok = match kind {
                    LevelKind::Squelch => client.set_squelch(lvl).is_ok(),
                    LevelKind::Volume => client.set_volume(lvl).is_ok(),
                };
                if ok {
                    let prefix = kind.response_prefix();
                    match kind {
                        LevelKind::Squelch => app.squelch = format!("{},{}", prefix, lvl),
                        LevelKind::Volume => app.volume = format!("{},{}", prefix, lvl),
                    }
                } else {
                    app.error = Some(format!("Failed to set {} level", kind.title()));
                }
            }
            app.input_mode = InputMode::Normal;
        }
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
        }
        _ => {}
    }
    false
}

fn handle_editing(
    app: &mut App,
    client: &mut ScannerClient,
    key: KeyEvent,
    idx: u32,
    edit_state: &mut EditState,
) -> bool {
    match key.code {
        KeyCode::Esc => {
            // Caller leaves input_mode as Normal (set by std::mem::replace)
        }
        KeyCode::Tab => {
            edit_state.active_field = match edit_state.active_field {
                EditField::Frequency => EditField::Name,
                EditField::Name => EditField::Frequency,
            };
        }
        KeyCode::Char(c) => match edit_state.active_field {
            EditField::Frequency => edit_state.frequency.push(c),
            EditField::Name => edit_state.name.push(c),
        },
        KeyCode::Backspace => match edit_state.active_field {
            EditField::Frequency => {
                edit_state.frequency.pop();
            }
            EditField::Name => {
                edit_state.name.pop();
            }
        },
        KeyCode::Enter => {
            // Validate user input before sending to the scanner.
            if let Some(freq) = Frequency::from_user_input(&edit_state.frequency) {
                let channel = Channel {
                    index: ChannelIndex::new(idx).unwrap(),
                    name: edit_state.name.clone(),
                    frequency: freq,
                    // The edit flow has no modulation field yet; AM is the
                    // scanner default.
                    modulation: Modulation::Am,
                };
                match client.set_channel(&channel) {
                    Ok(()) => app.channels[idx as usize] = Some(channel),
                    Err(e) => app.error = Some(format!("Failed to update channel: {e}")),
                }
            } else {
                app.error = Some("Invalid frequency input".to_string());
            }
            // Caller leaves input_mode as Normal (set by std::mem::replace)
        }
        _ => {}
    }
    false
}
