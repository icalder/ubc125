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
use crate::modes::ModeManager;
use crate::scanner::ScannerClient;
use crate::types::{BankMask, Channel, ChannelIndex, Frequency, Modulation, ScanStatus};

use super::renderer;

#[derive(Args)]
pub struct ConsoleArgs {
    #[arg(short, long, default_value_t = String::from("/dev/ttyACM0"))]
    pub console_device: String,
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
    pub(crate) fetch_queue: VecDeque<u32>,
    mode_manager: ModeManager,
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

        let model = client
            .send_command("MDL")
            .unwrap_or_else(|e| format!("Err: {}", e));
        let version = client
            .send_command("VER")
            .unwrap_or_else(|e| format!("Err: {}", e));
        let volume = client
            .get_volume()
            .unwrap_or_else(|e| format!("Err: {}", e));
        let squelch = client
            .get_squelch()
            .unwrap_or_else(|e| format!("Err: {}", e));

        // Fetch initial bank status via ModeManager.
        // Only query SCG if we successfully entered program mode.
        let mut mode_mgr = ModeManager::new();
        let scg_resp = if mode_mgr.ensure_program(client).is_ok() {
            let resp = client.send_command("SCG").unwrap_or_default();
            let _ = mode_mgr.ensure_monitor(client);
            resp
        } else {
            String::new()
        };

        let banks = BankMask::from_scanner_response(&scg_resp);

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
            mode_manager: mode_mgr,
            banks,
            input_mode: InputMode::Normal,
            table_state: TableState::default().with_selected(Some(0)),
            error: None,
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
            if self.channels[i as usize].is_none() && !self.fetch_queue.contains(&i) {
                self.fetch_queue.push_back(i);
            }
        }
    }

    /// Update a channel from a CIN response.
    fn update_channel(&mut self, response: &str) -> bool {
        if let Some(channel) = Channel::parse_cin(response) {
            let idx = channel.index.get() as usize;
            self.channels[idx] = Some(channel);
            true
        } else {
            false
        }
    }

    /// Update scan status from a GLG response.
    fn update_scan_status(&mut self, response: &str) {
        if let Some(status) = ScanStatus::parse_glg(response) {
            self.scan_status = status;
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
        self.mode_manager.is_prg()
    }
}

pub fn run(args: &ConsoleArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = ScannerClient::new(&args.console_device)?;
    let mut app = App::new(&mut client);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut last_poll = Instant::now();

    'main: loop {
        // Mode Management via ModeManager
        if app.selected_tab > 0 && !app.is_in_prg_mode() {
            if let Err(e) = app.mode_manager.ensure_program(&mut client) {
                app.error = Some(format!("Failed to enter program mode: {}", e));
            }
        } else if app.selected_tab == 0 && app.is_in_prg_mode() {
            if let Err(e) = app.mode_manager.ensure_monitor(&mut client) {
                app.error = Some(format!("Failed to return to monitor mode: {}", e));
            }
            app.fetch_queue.clear();
            app.error = None;
        }

        // Fetch Logic
        if app.is_in_prg_mode() {
            if let Some(idx) = app.fetch_queue.pop_front() {
                let resp = client
                    .send_command(&format!("CIN,{}", idx))
                    .unwrap_or_else(|e| format!("Err: {}", e));
                if !app.update_channel(&resp) {
                    app.fetch_queue.push_back(idx);
                }
            }
        } else if app.selected_tab == 0
            && last_poll.elapsed() >= Duration::from_millis(POLL_INTERVAL_MS)
        {
            let resp = client
                .send_command("GLG")
                .unwrap_or_else(|e| format!("Err: {}", e));
            app.update_scan_status(&resp);
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
        KeyCode::Char('s') if app.selected_tab == 0 && client.send_command("KEY,S,P").is_err() => {
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
        KeyCode::Char('h') if app.selected_tab == 0 && client.send_command("KEY,H,P").is_err() => {
            app.error = Some("Failed to hold scan".to_string());
        }
        KeyCode::Char(c) if app.selected_tab == 0 && c.is_ascii_digit() => {
            if let Some(digit) = c.to_digit(10) {
                let bank = if digit == 0 { NUM_BANKS as u32 } else { digit };
                let mut new_mask = app.banks.clone();
                new_mask.toggle(bank);
                let scg_cmd = new_mask.to_scanner_command();
                if app.mode_manager.ensure_program(client).is_ok() {
                    if client.send_command(&scg_cmd).is_ok() {
                        app.banks = new_mask;
                    } else {
                        app.error = Some("Failed to update bank mask".to_string());
                    }
                    let _ = app.mode_manager.ensure_monitor(client);
                } else {
                    app.error = Some("Failed to enter program mode".to_string());
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
            let cmd = format!("DCH,{}", idx);
            if client.send_command(&cmd).is_ok() {
                app.channels[idx as usize] = None;
                app.fetch_queue.push_back(idx);
            } else {
                app.error = Some("Failed to delete channel".to_string());
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
                let cmd = format!(
                    "CIN,{},{},{},AM,0,0,0,0",
                    idx,
                    edit_state.name,
                    freq.to_raw()
                );
                if client.send_command(&cmd).is_ok() {
                    app.channels[idx as usize] = Some(Channel {
                        index: ChannelIndex::new(idx).unwrap(),
                        name: edit_state.name.clone(),
                        frequency: freq,
                        modulation: Modulation::Am,
                    });
                } else {
                    app.error = Some("Failed to update channel".to_string());
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
