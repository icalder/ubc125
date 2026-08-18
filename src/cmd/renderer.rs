use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, Tabs},
};

use super::console::{App, EditField, EditState, InputMode, LevelKind};
use crate::constants::{
    BANK_STATUS_HEIGHT, CHANNELS_PER_BANK, EDIT_FIELD_HEIGHT, LIVE_SCAN_HEIGHT, MAX_LEVEL,
    PERCENT_BASE, POPUP_HEIGHT_CONFIRM, POPUP_HEIGHT_EDIT, POPUP_HEIGHT_LEVEL, POPUP_WIDTH_CONFIRM,
    POPUP_WIDTH_EDIT, POPUP_WIDTH_LEVEL, SCANNER_INFO_HEIGHT, STATUS_BAR_HEIGHT, TAB_BAR_HEIGHT,
    TABLE_COL_FREQ, TABLE_COL_INDEX, TABLE_COL_MOD, TABLE_COL_NAME,
};

/// Render the full application frame.
pub fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(TAB_BAR_HEIGHT),    // Tabs
                Constraint::Min(0),                    // Content
                Constraint::Length(STATUS_BAR_HEIGHT), // Help/Status
            ]
            .as_ref(),
        )
        .split(f.area());

    render_tabs(f, app, chunks[0]);
    render_content(f, app, chunks[1]);
    render_status(f, app, chunks[2]);
    render_popups(f, app);
}

/// Render the tab bar.
fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<&str> = app.tabs.iter().map(|t| t.as_str()).collect();
    let tabs = Tabs::new(titles)
        .select(app.selected_tab)
        .block(Block::default().borders(Borders::ALL).title("Tabs"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .divider("|");
    f.render_widget(tabs, area);
}

/// Render the main content area (Monitor or Bank view).
fn render_content(f: &mut Frame, app: &App, area: Rect) {
    if app.selected_tab == 0 {
        render_monitor_view(f, app, area);
    } else {
        render_bank_view(f, app, area);
    }
}

/// Render the Monitor (tab 0) view.
fn render_monitor_view(f: &mut Frame, app: &App, area: Rect) {
    let monitor_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(SCANNER_INFO_HEIGHT),
                Constraint::Length(LIVE_SCAN_HEIGHT),
                Constraint::Length(BANK_STATUS_HEIGHT), // Banks
            ]
            .as_ref(),
        )
        .split(area);

    // Scanner info block
    let info_text = format!(
        "Model:   {}\nVersion: {}\nVolume:  {}  [v]: Set\nSquelch: {}  [l]: Set",
        app.model, app.version, app.volume, app.squelch
    );
    let info_paragraph = Paragraph::new(info_text)
        .block(Block::default().title("Scanner Info").borders(Borders::ALL));
    f.render_widget(info_paragraph, monitor_chunks[0]);

    // Live scan block
    let scan_text = format!(
        "Bank:      {}\nFrequency: {} MHz\nChannel:   {}",
        app.scan_status.bank_display(),
        app.scan_status.frequency,
        app.scan_status.channel_name
    );

    let scan_style = if app.scan_status.signal_detected {
        Style::default()
            .bg(Color::Rgb(255, 165, 0))
            .fg(Color::Black)
    } else {
        Style::default()
    };

    let scan_paragraph = Paragraph::new(scan_text).block(
        Block::default()
            .title("Live Scan")
            .borders(Borders::ALL)
            .style(scan_style),
    );
    f.render_widget(scan_paragraph, monitor_chunks[1]);

    // Bank status block
    render_bank_status(f, app, monitor_chunks[2]);
}

/// Render the bank toggle status bar.
fn render_bank_status(f: &mut Frame, app: &App, area: Rect) {
    let mut bank_spans = vec![Span::raw("Banks: ")];
    for (i, active) in app.banks.iter() {
        let bank_num = i + 1;
        let style = if active {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        bank_spans.push(Span::styled(format!("[{}] ", bank_num % 10), style));
    }
    let banks_paragraph = Paragraph::new(Line::from(bank_spans)).block(
        Block::default()
            .title("Active Banks (Press 1-0 to toggle)")
            .borders(Borders::ALL),
    );
    f.render_widget(banks_paragraph, area);
}

/// Render the Bank (tab > 0) view.
fn render_bank_view(f: &mut Frame, app: &App, area: Rect) {
    let bank = app.selected_tab as u32;
    let start_idx = (bank - 1) * CHANNELS_PER_BANK + 1;
    let end_idx = bank * CHANNELS_PER_BANK;

    let mut rows = Vec::new();
    for i in start_idx..=end_idx {
        if let Some(chan) = &app.channels[i as usize] {
            rows.push(Row::new(vec![
                chan.index.to_string(),
                chan.name.clone(),
                chan.frequency.to_string(),
                chan.modulation.to_string(),
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
            Constraint::Length(TABLE_COL_INDEX),
            Constraint::Length(TABLE_COL_NAME),
            Constraint::Length(TABLE_COL_FREQ),
            Constraint::Length(TABLE_COL_MOD),
        ],
    )
    .header(
        Row::new(vec!["Idx", "Name", "Freq", "Mod"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Bank {}", bank)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol(">> ");
    // Render table with current selection state
    let mut state = app.table_state;
    f.render_stateful_widget(table, area, &mut state);
}

/// Render the bottom status/help bar.
fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let mode_str = if app.is_in_prg_mode() {
        "Remote (PRG)"
    } else {
        "Monitor"
    };
    let status_msg = if let Some(err) = &app.error {
        format!("ERROR: {}", err)
    } else if !app.fetch_queue.is_empty() {
        format!(
            "Loading... {} remaining ({})",
            app.fetch_queue.len(),
            mode_str
        )
    } else if app.selected_tab == 0 {
        app.scan_status.raw.clone()
    } else {
        format!("Ready ({})", mode_str)
    };

    let help_keys = if app.selected_tab == 0 {
        "Use Left/Right to switch tabs. 's': Scan, 'h': Hold, '1-0': Toggle Banks, 'q': Quit."
    } else {
        "Use Left/Right to switch tabs. Up/Down or j/k to navigate. 'e': Edit, 'd': Delete, 'q': Quit."
    };

    let help_text = Paragraph::new(format!("{}\nStatus: {}", help_keys, status_msg))
        .block(Block::default().title("Help").borders(Borders::ALL));
    f.render_widget(help_text, area);
}

/// Render overlay popups (ConfirmDelete, SetLevel, Editing).
fn render_popups(f: &mut Frame, app: &App) {
    if app.input_mode == InputMode::ConfirmDelete {
        render_confirm_delete_popup(f, app);
    }

    if let InputMode::SetLevel(kind) = &app.input_mode {
        render_set_level_popup(f, app, *kind);
    }

    if let InputMode::Editing(ref edit_state) = app.input_mode {
        render_edit_popup(f, edit_state);
    }
}

/// Render the "Confirm Delete" popup.
fn render_confirm_delete_popup(f: &mut Frame, app: &App) {
    let area = centered_rect(POPUP_WIDTH_CONFIRM, POPUP_HEIGHT_CONFIRM, f.area());
    f.render_widget(Clear, area);
    let idx = app.selected_channel_index();
    let text = format!(
        "\n  Are you sure you want to delete channel {}?\n\n  (y) Yes / (n) No",
        idx
    );
    let block = Block::default()
        .title("Confirm Delete")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Red));
    let paragraph = Paragraph::new(text).block(block);
    f.render_widget(paragraph, area);
}

/// Render the "Set Level" (Volume/Squelch) popup.
fn render_set_level_popup(f: &mut Frame, app: &App, kind: LevelKind) {
    let area = centered_rect(POPUP_WIDTH_LEVEL, POPUP_HEIGHT_LEVEL, f.area());
    f.render_widget(Clear, area);
    let text = format!(
        "\n  Enter {} Level (0-{}): {}",
        kind.title(),
        MAX_LEVEL,
        app.level_input
    );
    let block = Block::default()
        .title(format!("Set {}", kind.title()))
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Yellow));
    let paragraph = Paragraph::new(text).block(block);
    f.render_widget(paragraph, area);
}

/// Render the "Edit Channel" popup.
fn render_edit_popup(f: &mut Frame, edit_state: &EditState) {
    let area = centered_rect(POPUP_WIDTH_EDIT, POPUP_HEIGHT_EDIT, f.area());
    f.render_widget(Clear, area);

    let block = Block::default().title("Edit Channel").borders(Borders::ALL);
    f.render_widget(block, area);

    let inner_area = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(EDIT_FIELD_HEIGHT),
            Constraint::Length(EDIT_FIELD_HEIGHT),
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

    let freq_input = Paragraph::new(freq_text).block(
        Block::default()
            .title("Frequency (MHz)")
            .borders(Borders::ALL)
            .style(freq_display_style),
    );
    f.render_widget(freq_input, inner_area[0]);

    let name_input = Paragraph::new(edit_state.name.as_str()).block(
        Block::default()
            .title("Name")
            .borders(Borders::ALL)
            .style(name_style),
    );
    f.render_widget(name_input, inner_area[1]);

    let help = Paragraph::new("Tab: Switch Field | Enter: Save | Esc: Cancel");
    f.render_widget(help, inner_area[2]);
}

/// Calculate a centered rectangle within the given area.
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((PERCENT_BASE - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((PERCENT_BASE - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((PERCENT_BASE - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((PERCENT_BASE - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}
