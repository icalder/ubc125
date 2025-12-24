use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use clap::Args;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style, Color},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Tabs},
    Terminal,
};
use serialport::SerialPort;

#[derive(Args)]
pub struct ConsoleArgs {
    #[arg(short, long, default_value_t = String::from("/dev/ttyACM0"))]
    pub console_device: String,
}

#[derive(Clone, Debug)]
struct Channel {
    index: u32,
    name: String,
    frequency: String,
    modulation: String,
}

#[derive(Default, PartialEq)]
enum InputMode {
    #[default]
    Normal,
    Editing(EditState),
    ConfirmDelete,
}

#[derive(Clone, Default, PartialEq)]
enum EditField {
    #[default]
    Frequency,
    Name,
}

#[derive(Clone, Default, PartialEq)]
struct EditState {
    frequency: String,
    name: String,
    active_field: EditField,
}

struct ScanStatus {
    frequency: String,
    bank: String,
    channel_name: String,
    raw: String,
    signal_detected: bool,
}

impl Default for ScanStatus {
    fn default() -> Self {
        Self {
            frequency: "---".to_string(),
            bank: "-".to_string(),
            channel_name: "".to_string(),
            raw: "".to_string(),
            signal_detected: false,
        }
    }
}

struct App {
    model: String,
    version: String,
    volume: String,
    squelch: String,
    scan_status: ScanStatus,
    // Tab state
    tabs: Vec<String>,
    selected_tab: usize,
    // Channel data (Index 1-500)
    channels: Vec<Option<Channel>>,
    fetch_queue: VecDeque<u32>,
    in_prg_mode: bool,
    banks: Vec<bool>, // 10 banks (0-9 corresponds to Bank 1-10)
    input_mode: InputMode,
    table_state: TableState,
}

impl App {
    fn new(port: &mut Box<dyn SerialPort>) -> Self {
        let mut tabs = vec!["Monitor".to_string()];
        for i in 1..=10 {
            tabs.push(format!("Bank {}", i));
        }

        let model = send_command(port, "MDL");
        let version = send_command(port, "VER");
        let volume = send_command(port, "VOL");
        let squelch = send_command(port, "SQL");

        // Fetch initial bank status
        // Enter PRG mode temporarily
        let _ = send_command(port, "PRG");
        let scg_resp = send_command(port, "SCG");
        let _ = send_command(port, "EPG");
        let _ = send_command(port, "KEY,S,P");

        // Parse SCG: SCG,0101010101 (0=On, 1=Off)
        let mut banks = vec![true; 10]; // Default all on if parse fails
        let parts: Vec<&str> = scg_resp.split(',').collect();
        if parts.len() >= 2 && parts[0] == "SCG" {
            let mask = parts[1].trim();
            if mask.len() >= 10 {
                for (i, c) in mask.chars().take(10).enumerate() {
                    banks[i] = c == '0';
                }
            }
        }

        Self {
            model,
            version,
            volume,
            squelch,
            scan_status: ScanStatus::default(),
            tabs,
            selected_tab: 0,
            channels: vec![None; 501], // 1-based indexing, 500 channels
            fetch_queue: VecDeque::new(),
            in_prg_mode: false,
            banks,
            input_mode: InputMode::Normal,
            table_state: TableState::default().with_selected(Some(0)),
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
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= 49 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn previous_channel(&mut self) {
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    49
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn selected_channel_index(&self) -> u32 {
        if self.selected_tab == 0 {
            return 0;
        }
        let bank = self.selected_tab as u32;
        let row = self.table_state.selected().unwrap_or(0) as u32;
        (bank - 1) * 50 + row + 1
    }

    fn queue_channels_for_tab(&mut self) {
        if self.selected_tab == 0 {
            return;
        }
        let bank = self.selected_tab as u32; // Tab 1 = Bank 1
        let start_idx = (bank - 1) * 50 + 1;
        let end_idx = bank * 50;

        for i in start_idx..=end_idx {
            if self.channels[i as usize].is_none() {
                // Avoid adding duplicates if possible, or just push
                if !self.fetch_queue.contains(&i) {
                    self.fetch_queue.push_back(i);
                }
            }
        }
    }

    fn update_channel(&mut self, response: String) -> bool {
        // Expected format: CIN,[INDEX],[NAME],[FRQ],[MOD],...
        let parts: Vec<&str> = response.split(',').collect();
        if parts.len() >= 5 && parts[0] == "CIN" {
            if let Ok(idx) = parts[1].parse::<usize>() {
                if idx > 0 && idx <= 500 {
                    let mut freq = parts[3].to_string();
                    if freq.len() == 8 && freq.chars().all(|c| c.is_digit(10)) {
                        if freq == "00000000" {
                            freq = "".to_string();
                        } else {
                            let mhz = freq[0..4].trim_start_matches('0');
                            let mhz = if mhz.is_empty() { "0" } else { mhz };
                            let khz = freq[4..8].trim_end_matches('0');
                            if khz.is_empty() {
                                freq = format!("{}.0", mhz);
                            } else {
                                freq = format!("{}.{}", mhz, khz);
                            }
                        }
                    }

                    self.channels[idx] = Some(Channel {
                        index: idx as u32,
                        name: parts[2].to_string(),
                        frequency: freq,
                        modulation: parts[4].to_string(),
                    });
                    return true;
                }
            }
        }
        false
    }

    fn update_scan_status(&mut self, response: String) {
        self.scan_status.raw = response.clone();
        let parts: Vec<&str> = response.split(',').collect();
        if parts.len() >= 8 && parts[0] == "GLG" {
            // GLG,[Freq],[Modulation],,[Bank?],,,[Channel Name],
            // Example: GLG,01285500,AM,,0,,,GLOS APPR,
            
            // Format frequency: 01285500 -> 128.5500
            let raw_freq = parts[1];
            if raw_freq.len() >= 8 {
                let mhz = &raw_freq[0..4].trim_start_matches('0');
                let khz = &raw_freq[4..8];
                let mhz = if mhz.is_empty() { "0" } else { mhz };
                self.scan_status.frequency = format!("{}.{}", mhz, khz);
            } else {
                self.scan_status.frequency = raw_freq.to_string();
            }

            self.scan_status.channel_name = parts[7].trim().to_string();

            // Signal Status appears to be at index 8 (1 = Squelch Open/Detected)
            // Based on example: GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,
            if parts.len() > 8 {
                 self.scan_status.signal_detected = parts[8].trim() == "1";
            }
            
            // Calculate bank from Channel Index (index 11)
            // Example: GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,
            // Channel 52 is Bank 2. ((52-1)/50)+1 = 2.
            if parts.len() > 11 {
                 if let Ok(index) = parts[11].trim().parse::<u32>() {
                     if index > 0 {
                         let bank = ((index - 1) / 50) + 1;
                         self.scan_status.bank = bank.to_string();
                     }
                 }
            }
        }
    }

    fn get_scg_string(&self) -> String {
        let mut s = String::from("SCG,");
        for &b in &self.banks {
            s.push(if b { '0' } else { '1' });
        }
        s
    }
}

fn send_command(port: &mut Box<dyn SerialPort>, cmd: &str) -> String {
    let mut command = String::from(cmd);
    command.push('\r');
    if let Err(e) = port.write_all(command.as_bytes()) {
        return format!("Write Error: {}", e);
    }
    
    let mut response = String::new();
    let mut buf = [0u8; 1];
    let start = Instant::now();
    let timeout = Duration::from_millis(500);

    loop {
        if start.elapsed() > timeout {
             break;
        }
        match port.read(&mut buf) {
            Ok(n) if n > 0 => {
                let c = buf[0] as char;
                if c == '\r' {
                    break;
                }
                // Ignore newlines if they appear before \r (unlikely) or just append
                if c != '\n' {
                    response.push(c);
                }
            }
            Ok(_) => {},
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {},
            Err(e) => return format!("Read Error: {}", e),
        }
    }
    response.trim().to_string()
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}

pub fn run(args: &ConsoleArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Setup serial port
    let mut port = serialport::new(&args.console_device, 115_200)
        .timeout(Duration::from_millis(100))
        .open()?;

    // Clear any pending input
    let _ = port.clear(serialport::ClearBuffer::All);

    let mut app = App::new(&mut port);

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut last_poll = Instant::now();

    // Main loop
    loop {
        // Mode Management
        if app.selected_tab > 0 && !app.in_prg_mode {
            let _ = send_command(&mut port, "PRG");
            app.in_prg_mode = true;
        } else if app.selected_tab == 0 && app.in_prg_mode {
            let _ = send_command(&mut port, "EPG");
            // Automatically resume scanning when returning to Monitor
            let _ = send_command(&mut port, "KEY,S,P");
            app.in_prg_mode = false;
            app.fetch_queue.clear();
        }

        // Fetch Logic
        if app.in_prg_mode {
            if let Some(idx) = app.fetch_queue.pop_front() {
                let resp = send_command(&mut port, &format!("CIN,{}", idx));
                if !app.update_channel(resp) {
                     // Retry if failed (push to back)
                     app.fetch_queue.push_back(idx);
                }
            }
        } else {
            // Poll scanner status only in Monitor mode
            if app.selected_tab == 0 && last_poll.elapsed() >= Duration::from_millis(250) {
                let resp = send_command(&mut port, "GLG");
                app.update_scan_status(resp);
                last_poll = Instant::now();
            }
        }

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints(
                    [
                        Constraint::Length(3), // Tabs
                        Constraint::Min(0),    // Content
                        Constraint::Length(3), // Help/Status
                    ]
                    .as_ref(),
                )
                .split(f.area());

            let titles: Vec<&str> = app.tabs.iter().map(|t| t.as_str()).collect();
            let tabs = Tabs::new(titles)
                .select(app.selected_tab)
                .block(Block::default().borders(Borders::ALL).title("Tabs"))
                .highlight_style(Style::default().add_modifier(Modifier::BOLD))
                .divider("|");
            f.render_widget(tabs, chunks[0]);

            if app.selected_tab == 0 {
                // Monitor View
                let monitor_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints(
                        [
                            Constraint::Length(6),
                            Constraint::Length(6),
                            Constraint::Length(3), // Banks
                        ]
                        .as_ref(),
                    )
                    .split(chunks[1]);

                let info_text = format!(
                    "Model:   {}
Version: {}
Volume:  {}
Squelch: {}",
                    app.model, app.version, app.volume, app.squelch
                );
                let info_paragraph = Paragraph::new(info_text)
                    .block(Block::default().title("Scanner Info").borders(Borders::ALL));
                f.render_widget(info_paragraph, monitor_chunks[0]);

                let scan_text = format!(
                    "Bank:      {}
Frequency: {} MHz
Channel:   {}",
                    app.scan_status.bank,
                    app.scan_status.frequency,
                    app.scan_status.channel_name
                );

                let scan_style = if app.scan_status.signal_detected {
                    Style::default().bg(Color::Rgb(255, 165, 0)).fg(Color::Black)
                } else {
                    Style::default()
                };

                let scan_paragraph = Paragraph::new(scan_text)
                    .block(Block::default().title("Live Scan").borders(Borders::ALL).style(scan_style));
                f.render_widget(scan_paragraph, monitor_chunks[1]);

                // Bank Status
                let mut bank_spans = vec![Span::raw("Banks: ")];
                for (i, &active) in app.banks.iter().enumerate() {
                    let bank_num = i + 1;
                    let style = if active {
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    bank_spans.push(Span::styled(format!("[{}] ", bank_num % 10), style));
                }
                let banks_paragraph = Paragraph::new(Line::from(bank_spans))
                    .block(Block::default().title("Active Banks (Press 1-0 to toggle)").borders(Borders::ALL));
                f.render_widget(banks_paragraph, monitor_chunks[2]);

            } else {
                // Bank View
                let bank = app.selected_tab as u32;
                let start_idx = (bank - 1) * 50 + 1;
                let end_idx = bank * 50;
                
                let mut rows = Vec::new();
                for i in start_idx..=end_idx {
                    if let Some(chan) = &app.channels[i as usize] {
                        rows.push(Row::new(vec![
                            chan.index.to_string(),
                            chan.name.clone(),
                            chan.frequency.clone(),
                            chan.modulation.clone(),
                        ]));
                    } else {
                        rows.push(Row::new(vec![
                            i.to_string(),
                            "Loading...".to_string(),
                            "".to_string(),
                            "".to_string(),
                        ]));
                    }
                }
                
                let table = Table::new(
                    rows,
                    [
                        Constraint::Length(5),
                        Constraint::Length(20),
                        Constraint::Length(10),
                        Constraint::Length(5),
                    ]
                )
                .header(Row::new(vec!["Idx", "Name", "Freq", "Mod"]).style(Style::default().add_modifier(Modifier::BOLD)))
                .block(Block::default().borders(Borders::ALL).title(format!("Bank {}", bank)))
                .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol(">> ");
                f.render_stateful_widget(table, chunks[1], &mut app.table_state);
            }

            let mode_str = if app.in_prg_mode { "Remote (PRG)" } else { "Monitor" };
            let status_msg = if !app.fetch_queue.is_empty() {
                format!("Loading... {} remaining ({})", app.fetch_queue.len(), mode_str)
            } else {
                if app.selected_tab == 0 {
                    app.scan_status.raw.clone()
                } else {
                    format!("Ready ({})", mode_str)
                }
            };

            let help_keys = if app.selected_tab == 0 {
                "Use Left/Right to switch tabs. 's': Scan, 'h': Hold, '1-0': Toggle Banks, 'q': Quit."
            } else {
                "Use Left/Right to switch tabs. Up/Down or j/k to navigate. 'e': Edit, 'd': Delete, 'q': Quit."
            };

            let help_text = Paragraph::new(format!("{}\nStatus: {}", help_keys, status_msg))
                .block(Block::default().title("Help").borders(Borders::ALL));
             f.render_widget(help_text, chunks[2]);

            if app.input_mode == InputMode::ConfirmDelete {
                let area = centered_rect(60, 20, f.area());
                f.render_widget(Clear, area);
                let idx = app.selected_channel_index();
                let text = format!("\n  Are you sure you want to delete channel {}?\n\n  (y) Yes / (n) No", idx);
                let block = Block::default().title("Confirm Delete").borders(Borders::ALL).style(Style::default().fg(Color::Red));
                let paragraph = Paragraph::new(text).block(block);
                f.render_widget(paragraph, area);
            }

            if let InputMode::Editing(edit_state) = &app.input_mode {
                let area = centered_rect(60, 40, f.area());
                f.render_widget(Clear, area);

                let block = Block::default().title("Edit Channel").borders(Borders::ALL);
                f.render_widget(block, area);

                let inner_area = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(2)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Length(3),
                        Constraint::Min(0),
                    ])
                    .split(area);

                let freq_style = if edit_state.active_field == EditField::Frequency {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                };
                let name_style = if edit_state.active_field == EditField::Name {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                };

                let (freq_text, freq_display_style) = if edit_state.frequency.is_empty() {
                    ("118.100", Style::default().fg(Color::DarkGray))
                } else {
                    (edit_state.frequency.as_str(), freq_style)
                };

                let freq_input = Paragraph::new(freq_text)
                    .block(Block::default().title("Frequency (MHz)").borders(Borders::ALL).style(freq_display_style));
                f.render_widget(freq_input, inner_area[0]);

                let name_input = Paragraph::new(edit_state.name.as_str())
                    .block(Block::default().title("Name").borders(Borders::ALL).style(name_style));
                f.render_widget(name_input, inner_area[1]);

                let help = Paragraph::new("Tab: Switch Field | Enter: Save | Esc: Cancel");
                f.render_widget(help, inner_area[2]);
            }
        })?;

        // Poll for input
        let poll_timeout = if !app.fetch_queue.is_empty() {
            Duration::from_millis(1) 
        } else {
            Duration::from_millis(50)
        };

        if event::poll(poll_timeout)? {
            if let Event::Key(key) = event::read()? {
                let idx = app.selected_channel_index();
                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Right => app.next_tab(),
                        KeyCode::Left => app.previous_tab(),
                        KeyCode::Down | KeyCode::Char('j') if app.selected_tab > 0 => {
                            app.next_channel();
                        }
                        KeyCode::Up | KeyCode::Char('k') if app.selected_tab > 0 => {
                            app.previous_channel();
                        }
                        KeyCode::Char('d') if app.selected_tab > 0 => {
                            app.input_mode = InputMode::ConfirmDelete;
                        }
                        KeyCode::Char('e') | KeyCode::Enter if app.selected_tab > 0 => {
                            let (freq, name) = if let Some(chan) = &app.channels[idx as usize] {
                                (chan.frequency.clone(), chan.name.clone())
                            } else {
                                ("".to_string(), "".to_string())
                            };
                            app.input_mode = InputMode::Editing(EditState {
                                frequency: freq,
                                name: name,
                                active_field: EditField::Frequency,
                            });
                        }
                        KeyCode::Char('s') if app.selected_tab == 0 => {
                            let _ = send_command(&mut port, "KEY,S,P");
                        }
                        KeyCode::Char('h') if app.selected_tab == 0 => {
                            let _ = send_command(&mut port, "KEY,H,P");
                        }
                        KeyCode::Char(c) if app.selected_tab == 0 && c.is_digit(10) => {
                            if let Some(digit) = c.to_digit(10) {
                                // 1->0, 2->1, ... 0->9
                                let bank_idx = if digit == 0 { 9 } else { digit - 1 } as usize;
                                if bank_idx < 10 {
                                    app.banks[bank_idx] = !app.banks[bank_idx];
                                    let scg_cmd = app.get_scg_string();
                                    // Apply change
                                    let _ = send_command(&mut port, "PRG");
                                    let _ = send_command(&mut port, &scg_cmd);
                                    let _ = send_command(&mut port, "EPG");
                                    let _ = send_command(&mut port, "KEY,S,P");
                                }
                            }
                        }
                        _ => {}
                    },
                    InputMode::ConfirmDelete => match key.code {
                        KeyCode::Char('y') => {
                            let cmd = format!("DCH,{}", idx);
                            let _ = send_command(&mut port, &cmd);
                            app.channels[idx as usize] = None;
                            app.fetch_queue.push_back(idx);
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                        }
                        _ => {}
                    },
                    InputMode::Editing(ref mut edit_state) => match key.code {
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
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
                            let raw_freq = if edit_state.frequency.contains('.') {
                                let parts: Vec<&str> = edit_state.frequency.split('.').collect();
                                let mut mhz = parts[0].to_string();
                                let mut khz = if parts.len() > 1 {
                                    parts[1].to_string()
                                } else {
                                    "".to_string()
                                };

                                // Pad MHz to 4 digits with leading zeros
                                while mhz.len() < 4 {
                                    mhz.insert(0, '0');
                                }
                                if mhz.len() > 4 {
                                    mhz.truncate(4);
                                }

                                // Pad KHz to 4 digits with trailing zeros
                                while khz.len() < 4 {
                                    khz.push('0');
                                }
                                if khz.len() > 4 {
                                    khz.truncate(4);
                                }
                                format!("{}{}", mhz, khz)
                            } else if edit_state.frequency.len() >= 7 {
                                // Assume raw format if long and no dot
                                let mut f = edit_state.frequency.clone();
                                while f.len() < 8 {
                                    f.insert(0, '0');
                                }
                                if f.len() > 8 {
                                    f.truncate(8);
                                }
                                f
                            } else if !edit_state.frequency.is_empty() {
                                // Short input without dot, assume MHz
                                let mut mhz = edit_state.frequency.clone();
                                while mhz.len() < 4 {
                                    mhz.insert(0, '0');
                                }
                                format!("{}0000", mhz)
                            } else {
                                "".to_string()
                            };

                            let cmd =
                                format!("CIN,{},{},{},AM,0,0,0,0", idx, edit_state.name, raw_freq);
                            let _ = send_command(&mut port, &cmd);

                            // Update local state
                            app.channels[idx as usize] = Some(Channel {
                                index: idx,
                                name: edit_state.name.clone(),
                                frequency: edit_state.frequency.clone(),
                                modulation: "AM".to_string(),
                            });

                            app.input_mode = InputMode::Normal;
                        }
                        _ => {}
                    },
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_glg_response() {
        let mut app = App {
            model: "".into(),
            version: "".into(),
            volume: "".into(),
            squelch: "".into(),
            scan_status: ScanStatus::default(),
            tabs: vec![],
            selected_tab: 0,
            channels: vec![],
            fetch_queue: VecDeque::new(),
            in_prg_mode: false,
            banks: vec![true; 10],
            input_mode: InputMode::Normal,
            table_state: TableState::default(),
        };

        // Example from SCANNER-COMMANDS.md: GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,
        app.update_scan_status("GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,".to_string());
        
        assert_eq!(app.scan_status.frequency, "123.9750");
        assert_eq!(app.scan_status.bank, "2");
        assert_eq!(app.scan_status.channel_name, "BHX RADAR");
    }

    #[test]
    fn test_parse_glg_low_frequency() {
        let mut app = App {
            model: "".into(),
            version: "".into(),
            volume: "".into(),
            squelch: "".into(),
            scan_status: ScanStatus::default(),
            tabs: vec![],
            selected_tab: 0,
            channels: vec![],
            fetch_queue: VecDeque::new(),
            in_prg_mode: false,
            banks: vec![true; 10],
            input_mode: InputMode::Normal,
            table_state: TableState::default(),
        };

        // Test with a frequency < 100MHz (padding check)
        app.update_scan_status("GLG,00881000,FM,,0,,,BBC R2,1,0,,1,".to_string());
        
        assert_eq!(app.scan_status.frequency, "88.1000");
        assert_eq!(app.scan_status.bank, "1");
        assert_eq!(app.scan_status.channel_name, "BBC R2");
    }

    #[test]
    fn test_parse_glg_signal_detected() {
        let mut app = App {
            model: "".into(),
            version: "".into(),
            volume: "".into(),
            squelch: "".into(),
            scan_status: ScanStatus::default(),
            tabs: vec![],
            selected_tab: 0,
            channels: vec![],
            fetch_queue: VecDeque::new(),
            in_prg_mode: false,
            banks: vec![true; 10],
            input_mode: InputMode::Normal,
            table_state: TableState::default(),
        };

        // Case 1: Signal Detected (Index 8 = 1)
        // Example: GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,
        app.update_scan_status("GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,".to_string());
        assert!(app.scan_status.signal_detected);
        assert_eq!(app.scan_status.channel_name, "BHX RADAR");

        // Case 2: No Signal (Index 8 = 0)
        app.update_scan_status("GLG,01239750,AM,,0,,,QUIET,0,0,,52,".to_string());
        assert!(!app.scan_status.signal_detected);
    }
}
