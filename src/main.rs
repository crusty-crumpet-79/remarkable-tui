use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use reqwest::{multipart, Client};
use serde::Deserialize;
use std::{
    error::Error,
    io,
    path::Path,
    time::Duration,
};
use tokio::sync::mpsc;

// --- Data Structures ---

#[derive(Debug, Deserialize, Clone)]
struct Item {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "VissibleName")]
    visible_name: String,
    #[serde(rename = "Type")]
    item_type: String,
}

impl Item {
    fn is_folder(&self) -> bool {
        self.item_type == "CollectionType"
    }
}

enum InputMode {
    Normal,
    Uploading,
}

enum AppMessage {
    DocumentsFetched(Vec<Item>), // items
    DownloadComplete(String),
    UploadComplete(String),
    Error(String),
}

struct AppLogic {
    items: Vec<Item>,
    state: ListState,
    current_guid: Option<String>,
    history: Vec<Option<String>>, // Stack of previous locations
    input_mode: InputMode,
    input_buffer: String,
    status_msg: String,
    client: Client,
    tx: mpsc::Sender<AppMessage>,
}

impl AppLogic {
    fn new(tx: mpsc::Sender<AppMessage>) -> Self {
        Self {
            items: Vec::new(),
            state: ListState::default(),
            current_guid: None,
            history: Vec::new(),
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            status_msg: "Ready.".into(),
            client: Client::new(),
            tx,
        }
    }

    fn next(&mut self) {
        if self.items.is_empty() { return; }
        let i = match self.state.selected() {
            Some(i) => if i >= self.items.len() - 1 { 0 } else { i + 1 },
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        if self.items.is_empty() { return; }
        let i = match self.state.selected() {
            Some(i) => if i == 0 { self.items.len() - 1 } else { i - 1 },
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn refresh(&mut self) {
        self.status_msg = "Loading...".into();
        let client = self.client.clone();
        let guid = self.current_guid.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match fetch_documents(&client, &guid).await {
                Ok(items) => {
                    let _ = tx.send(AppMessage::DocumentsFetched(items)).await;
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::Error(format!("Error: {}", e))).await;
                }
            }
        });
    }

    fn enter(&mut self) {
        if let Some(i) = self.state.selected() {
            if let Some(item) = self.items.get(i) {
                if item.is_folder() {
                    self.history.push(self.current_guid.clone());
                    self.current_guid = Some(item.id.clone());
                    self.state.select(None);
                    self.refresh();
                }
            }
        }
    }

    fn go_back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.current_guid = prev;
            self.state.select(None);
            self.refresh();
        } else {
            self.status_msg = "Already at root.".into();
        }
    }

    fn download(&mut self) {
        if let Some(i) = self.state.selected() {
            if let Some(item) = self.items.get(i) {
                if !item.is_folder() {
                    let client = self.client.clone();
                    let id = item.id.clone();
                    let name = item.visible_name.clone();
                    let tx = self.tx.clone();
                    self.status_msg = format!("Downloading {}...", name);
                    
                    tokio::spawn(async move {
                        match download_file(&client, &id, &name).await {
                            Ok(_) => {
                                let _ = tx.send(AppMessage::DownloadComplete(name)).await;
                            },
                            Err(e) => {
                                let _ = tx.send(AppMessage::Error(format!("Download failed: {}", e))).await;
                            }
                        }
                    });
                } else {
                    self.status_msg = "Cannot download a folder.".into();
                }
            }
        }
    }

    fn start_upload(&mut self) {
        self.input_mode = InputMode::Uploading;
        self.input_buffer.clear();
        self.status_msg = "Enter file path to upload:".into();
    }

    fn cancel_upload(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.status_msg = "Upload cancelled.".into();
    }

    fn confirm_upload(&mut self) {
        let path_str = self.input_buffer.trim().to_string();
        if path_str.is_empty() {
            self.status_msg = "Path cannot be empty.".into();
            return;
        }
        let path = Path::new(&path_str);
        if !path.exists() {
            self.status_msg = "File does not exist.".into();
            return;
        }

        self.input_mode = InputMode::Normal;
        self.status_msg = format!("Uploading {}...", path_str);
        
        let client = self.client.clone();
        let current_guid = self.current_guid.clone();
        let tx = self.tx.clone();
        let file_path = path_str.clone();

        tokio::spawn(async move {
            // 1. Fetch current list to ensure target (per requirements)
            if let Err(e) = fetch_documents(&client, &current_guid).await {
                 let _ = tx.send(AppMessage::Error(format!("Upload pre-check failed: {}", e))).await;
                 return;
            }
            
            // 2. Upload
            match upload_file(&client, &file_path).await {
                Ok(_) => {
                     let _ = tx.send(AppMessage::UploadComplete(file_path)).await;
                },
                Err(e) => {
                     let _ = tx.send(AppMessage::Error(format!("Upload failed: {}", e))).await;
                }
            }
        });
    }
}

// --- Network Helpers ---

const BASE_URL: &str = "http://10.11.99.1";

async fn fetch_documents(client: &Client, guid: &Option<String>) -> Result<Vec<Item>> {
    let url = match guid {
        Some(id) => format!("{}/documents/{}", BASE_URL, id),
        None => format!("{}/documents/", BASE_URL),
    };

    let resp = client.get(&url).send().await?;
    let items: Vec<Item> = resp.json().await?;
    Ok(items)
}

async fn download_file(client: &Client, id: &str, name: &str) -> Result<()> {
    let url = format!("{}/download/{}/pdf", BASE_URL, id);
    let resp = client.get(&url).send().await?;
    
    // Sanitize filename
    let safe_name = name.replace(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '_', "_");
    let file_name = if safe_name.ends_with(".pdf") { safe_name } else { format!("{}.pdf", safe_name) };

    let mut file = tokio::fs::File::create(&file_name).await?;
    let mut stream = resp.bytes_stream();

    while let Some(item) = stream.next().await {
        let chunk = item?;
        use tokio::io::AsyncWriteExt;
        file.write_all(&chunk).await?;
    }
    
    Ok(())
}

async fn upload_file(client: &Client, path_str: &str) -> Result<()> {
    let path = Path::new(path_str);
    let file_name = path.file_name().ok_or_else(|| anyhow::anyhow!("Invalid filename"))?
        .to_string_lossy().to_string();

    let file_bytes = tokio::fs::read(path).await?;
    
    // Create multipart form
    // The API expects file=@path. 
    // In reqwest multipart, we add a part.
    let part = multipart::Part::bytes(file_bytes)
        .file_name(file_name);
    
    let form = multipart::Form::new()
        .part("file", part);

    let url = format!("{}/upload", BASE_URL);
    let resp = client.post(&url)
        .multipart(form)
        .send()
        .await?;
        
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Upload failed with status: {}", resp.status()))
    }
}

// --- Main ---

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create App
    let (tx, mut rx) = mpsc::channel(10);
    let mut app = AppLogic::new(tx);
    
    // Initial fetch
    app.refresh();

    let res = run_app(&mut terminal, &mut app, &mut rx).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

async fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut AppLogic,
    rx: &mut mpsc::Receiver<AppMessage>,
) -> io::Result<()> {
    let tick_rate = Duration::from_millis(100);
    let mut last_tick = std::time::Instant::now();

    loop {
        terminal.draw(|f| ui(f, app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.input_mode {
                        InputMode::Normal => match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Char('j') | KeyCode::Down => app.next(),
                            KeyCode::Char('k') | KeyCode::Up => app.previous(),
                            KeyCode::Char('l') | KeyCode::Enter => app.enter(),
                            KeyCode::Char('h') | KeyCode::Backspace | KeyCode::Left => app.go_back(),
                            KeyCode::Char('d') => app.download(),
                            KeyCode::Char('u') => app.start_upload(),
                            KeyCode::Char('r') => app.refresh(),
                            _ => {}
                        },
                        InputMode::Uploading => match key.code {
                            KeyCode::Enter => app.confirm_upload(),
                            KeyCode::Esc => app.cancel_upload(),
                            KeyCode::Char(c) => app.input_buffer.push(c),
                            KeyCode::Backspace => { app.input_buffer.pop(); },
                            _ => {}
                        },
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = std::time::Instant::now();
        }

        // Process async messages
        while let Ok(msg) = rx.try_recv() {
            match msg {
                AppMessage::DocumentsFetched(items) => {
                    app.items = items;
                    app.status_msg = format!("Loaded {} items.", app.items.len());
                },
                AppMessage::DownloadComplete(name) => {
                    app.status_msg = format!("Downloaded {}.", name);
                },
                AppMessage::UploadComplete(name) => {
                    app.status_msg = format!("Uploaded {}. Refreshing...", name);
                    app.refresh();
                },
                AppMessage::Error(e) => {
                    app.status_msg = format!("Error: {}", e);
                },

            }
        }
    }
}

fn ui(f: &mut Frame, app: &mut AppLogic) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    // List
    let items: Vec<ListItem> = app
        .items
        .iter()
        .map(|i| {
            let icon = if i.is_folder() { "📁" } else { "📄" };
            let content = format!("{} {}", icon, i.visible_name);
            ListItem::new(Line::from(content))
        })
        .collect();

    let items_list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(
            match &app.current_guid {
                Some(id) => format!(" Documents / {} ", id),
                None => " Documents / (Root) ".to_string(),
            }
        ))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray))
        .highlight_symbol("> ");

    f.render_stateful_widget(items_list, chunks[0], &mut app.state);

    // Status Bar
    let status_style = match app.input_mode {
        InputMode::Uploading => Style::default().bg(Color::Blue).fg(Color::White),
        InputMode::Normal => Style::default().bg(Color::White).fg(Color::Black),
    };
    let status = Paragraph::new(app.status_msg.clone()).style(status_style);
    f.render_widget(status, chunks[1]);

    // Input Modal
    if let InputMode::Uploading = app.input_mode {
        let area = centered_rect(60, 20, f.area());
        f.render_widget(Clear, area); // Clear background

        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(" Upload File ")
            .style(Style::default().bg(Color::Black));
        
        let input_text = Paragraph::new(app.input_buffer.clone())
            .block(input_block)
            .wrap(Wrap { trim: true });
            
        f.render_widget(input_text, area);
    }
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