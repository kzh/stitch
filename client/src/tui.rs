use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame, Terminal,
};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

use crate::CliContext;
use proto::stitch::*;

pub struct App {
    pub channels: Vec<Channel>,
    pub selected_tab: usize,
    pub channel_list_state: ListState,
    pub search_query: String,
    pub is_searching: bool,
    pub status_message: Option<(String, Instant)>,
    pub show_help: bool,
    pub loading: bool,
    pub input_mode: InputMode,
    pub input_buffer: String,
    ctx: Arc<Mutex<CliContext>>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum InputMode {
    Normal,
    AddingChannel,
    ConfirmingDelete,
}

impl App {
    pub fn new(ctx: CliContext) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            channels: Vec::new(),
            selected_tab: 0,
            channel_list_state: list_state,
            search_query: String::new(),
            is_searching: false,
            status_message: None,
            show_help: false,
            loading: true,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            ctx: Arc::new(Mutex::new(ctx)),
        }
    }

    pub async fn load_channels(&mut self) -> Result<()> {
        self.loading = true;

        let channels_result = {
            let ctx = self.ctx.lock().await;
            let mut client = ctx.client.clone();

            let request = ctx.create_request(ListChannelsRequest {});

            client.list_channels(request).await
        };

        match channels_result {
            Ok(response) => {
                self.channels = response.into_inner().channels;
                self.loading = false;
                self.set_status("Channels loaded successfully");
                Ok(())
            }
            Err(e) => {
                self.loading = false;
                self.set_status(&format!("Error loading channels: {}", e.message()));
                Err(e.into())
            }
        }
    }

    pub fn set_status(&mut self, message: &str) {
        self.status_message = Some((message.to_string(), Instant::now()));
    }

    pub fn filtered_channels(&self) -> Vec<&Channel> {
        if self.search_query.is_empty() {
            self.channels.iter().collect()
        } else {
            self.channels
                .iter()
                .filter(|c| {
                    c.name
                        .to_lowercase()
                        .contains(&self.search_query.to_lowercase())
                })
                .collect()
        }
    }

    pub fn next_channel(&mut self) {
        let channels = self.filtered_channels();
        if channels.is_empty() {
            return;
        }

        let i = match self.channel_list_state.selected() {
            Some(i) => {
                if i >= channels.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.channel_list_state.select(Some(i));
    }

    pub fn previous_channel(&mut self) {
        let channels = self.filtered_channels();
        if channels.is_empty() {
            return;
        }

        let i = match self.channel_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    channels.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.channel_list_state.select(Some(i));
    }

    pub async fn track_channel(&mut self, name: String) -> Result<()> {
        let result = {
            let ctx = self.ctx.lock().await;
            let mut client = ctx.client.clone();

            let request = ctx.create_request(TrackChannelRequest { name: name.clone() });

            client.track_channel(request).await
        };

        match result {
            Ok(_) => {
                self.set_status(&format!("Successfully tracked channel: {}", name));
                self.load_channels().await?;
                Ok(())
            }
            Err(e) => {
                if e.code() == tonic::Code::AlreadyExists {
                    self.set_status(&format!("Channel '{}' is already being tracked", name));
                } else {
                    self.set_status(&format!("Failed to track channel: {}", e.message()));
                }
                Err(e.into())
            }
        }
    }

    pub async fn untrack_channel(&mut self, name: String) -> Result<()> {
        let result = {
            let ctx = self.ctx.lock().await;
            let mut client = ctx.client.clone();

            let request = ctx.create_request(UntrackChannelRequest { name: name.clone() });

            client.untrack_channel(request).await
        };

        match result {
            Ok(_) => {
                self.set_status(&format!("Successfully untracked channel: {}", name));
                self.load_channels().await?;
                Ok(())
            }
            Err(e) => {
                self.set_status(&format!("Failed to untrack channel: {}", e.message()));
                Err(e.into())
            }
        }
    }
}

pub async fn run_tui(ctx: CliContext) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(ctx);

    let _ = app.load_channels().await;

    let res = run_app(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    res
}

async fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') if !app.is_searching => return Ok(()),
                        KeyCode::Char('?') => app.show_help = !app.show_help,
                        KeyCode::Tab => {
                            app.selected_tab = (app.selected_tab + 1) % 2;
                        }
                        KeyCode::Char('/') if !app.is_searching => {
                            app.is_searching = true;
                            app.search_query.clear();
                        }
                        KeyCode::Esc if app.is_searching => {
                            app.is_searching = false;
                            app.search_query.clear();
                        }
                        KeyCode::Char(c) if app.is_searching => {
                            app.search_query.push(c);
                        }
                        KeyCode::Backspace if app.is_searching => {
                            app.search_query.pop();
                        }
                        KeyCode::Enter if app.is_searching => {
                            app.is_searching = false;
                        }
                        KeyCode::Down | KeyCode::Char('j') if !app.is_searching => {
                            app.next_channel();
                        }
                        KeyCode::Up | KeyCode::Char('k') if !app.is_searching => {
                            app.previous_channel();
                        }
                        KeyCode::Char('r') if !app.is_searching => {
                            let _ = app.load_channels().await;
                        }
                        KeyCode::Char('a') if !app.is_searching => {
                            app.input_mode = InputMode::AddingChannel;
                            app.input_buffer.clear();
                        }
                        KeyCode::Char('d') if !app.is_searching => {
                            if let Some(i) = app.channel_list_state.selected() {
                                if app.filtered_channels().get(i).is_some() {
                                    app.input_mode = InputMode::ConfirmingDelete;
                                }
                            }
                        }
                        _ => {}
                    },
                    InputMode::AddingChannel => match key.code {
                        KeyCode::Enter => {
                            let name = app.input_buffer.clone();
                            if !name.trim().is_empty() {
                                app.input_mode = InputMode::Normal;
                                let _ = app.track_channel(name).await;
                            }
                        }
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                            app.input_buffer.clear();
                        }
                        KeyCode::Char(c) => {
                            app.input_buffer.push(c);
                        }
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        _ => {}
                    },
                    InputMode::ConfirmingDelete => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            if let Some(i) = app.channel_list_state.selected() {
                                if let Some(channel) = app.filtered_channels().get(i) {
                                    let name = channel.name.clone();
                                    app.input_mode = InputMode::Normal;
                                    let _ = app.untrack_channel(name).await;
                                }
                            }
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                        }
                        _ => {}
                    },
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Content
            Constraint::Length(3), // Footer
        ])
        .split(f.area());

    let header = Tabs::new(vec![Line::from("Channels"), Line::from("Settings")])
        .block(Block::default().borders(Borders::ALL).title(" Stitch TUI "))
        .select(app.selected_tab)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(header, chunks[0]);

    match app.selected_tab {
        0 => render_channels_tab(f, app, chunks[1]),
        1 => render_settings_tab(f, app, chunks[1]),
        _ => {}
    }

    let footer = match app.input_mode {
        InputMode::AddingChannel => Paragraph::new(format!("Add channel: {}_", app.input_buffer))
            .style(Style::default().fg(Color::Yellow))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Press Enter to add, Esc to cancel "),
            ),
        InputMode::ConfirmingDelete => {
            if let Some(i) = app.channel_list_state.selected() {
                if let Some(channel) = app.filtered_channels().get(i) {
                    Paragraph::new(format!(
                        "Delete '{}'? Press Y to confirm, N to cancel",
                        channel.name
                    ))
                    .style(Style::default().fg(Color::Red))
                    .block(Block::default().borders(Borders::ALL))
                } else {
                    render_help_footer(app)
                }
            } else {
                render_help_footer(app)
            }
        }
        InputMode::Normal => {
            if app.is_searching {
                Paragraph::new(format!("Search: {}_", app.search_query))
                    .style(Style::default().fg(Color::Yellow))
                    .block(Block::default().borders(Borders::ALL))
            } else if let Some((msg, time)) = &app.status_message {
                if time.elapsed() < Duration::from_secs(5) {
                    Paragraph::new(msg.as_str())
                        .style(Style::default().fg(Color::Green))
                        .block(Block::default().borders(Borders::ALL))
                } else {
                    render_help_footer(app)
                }
            } else {
                render_help_footer(app)
            }
        }
    };
    f.render_widget(footer, chunks[2]);

    if app.show_help {
        render_help_overlay(f);
    }
}

fn render_help_footer(app: &App) -> Paragraph<'static> {
    let help_text = if app.loading {
        "Loading..."
    } else {
        "[q] Quit | [Tab] Switch tabs | [/] Search | [?] Help | [r] Refresh"
    };

    Paragraph::new(help_text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL))
        .alignment(Alignment::Center)
}

fn render_channels_tab(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    let channels = app.filtered_channels();
    let items: Vec<ListItem> = channels
        .iter()
        .map(|c| {
            let content = Line::from(vec![
                Span::raw(&c.name),
                Span::raw(" "),
                Span::styled(
                    format!("(ID: {})", c.id),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            ListItem::new(content)
        })
        .collect();

    let total_count = app.channels.len();
    let filtered_count = channels.len();
    let title_text = if app.search_query.is_empty() {
        format!(" Channels ({}) ", total_count)
    } else {
        format!(" Channels ({}/{}) ", filtered_count, total_count)
    };

    let channels_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(title_text))
                .title_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    f.render_stateful_widget(
        channels_list,
        chunks[0],
        &mut app.channel_list_state.clone(),
    );

    if let Some(selected) = app.channel_list_state.selected() {
        if let Some(channel) = channels.get(selected) {
            render_channel_details(f, channel, chunks[1]);
        }
    }
}

fn render_channel_details(f: &mut Frame, channel: &Channel, area: Rect) {
    let details = vec![
        Line::from(vec![
            Span::styled("ID: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(channel.id.to_string()),
        ]),
        Line::from(vec![
            Span::styled("Name: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&channel.name),
        ]),
    ];

    let all_lines = details;

    let paragraph = Paragraph::new(all_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Channel Details ")
                .title_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

fn render_settings_tab(f: &mut Frame, _app: &App, area: Rect) {
    let text = vec![
        Line::from("Settings management coming soon!"),
        Line::from(""),
        Line::from("Configuration file: ~/.config/stitch/config.toml"),
    ];

    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Settings ")
                .title_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

fn render_help_overlay(f: &mut Frame) {
    let area = centered_rect(60, 60, f.area());

    let help_text = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "Navigation",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from("  ↑/k     - Move up"),
        Line::from("  ↓/j     - Move down"),
        Line::from("  Tab     - Switch tabs"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Channel Management",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from("  a       - Add new channel"),
        Line::from("  d       - Delete selected channel"),
        Line::from("  r       - Refresh channel list"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Search",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from("  /       - Start search"),
        Line::from("  Esc     - Cancel search"),
        Line::from("  Enter   - Confirm search"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "General",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from("  ?       - Toggle this help"),
        Line::from("  q       - Quit application"),
        Line::from(""),
    ];

    let help = Paragraph::new(help_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Help ")
                .title_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .alignment(Alignment::Left);

    f.render_widget(help, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
