use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{future::BoxFuture, StreamExt};
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

fn sanitize_filename(name: &str, is_folder: bool) -> String {
    let safe_name = name.replace(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '_', "_");
    if !is_folder && !safe_name.ends_with(".pdf") {
        format!("{}.pdf", safe_name)
    } else {
        safe_name
    }
}

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
    Downloading,
}

enum AppMessage {
    DocumentsFetched(Vec<Item>), // items
    DownloadComplete(String, String), // name, path
    UploadComplete(String),
    Error(String),
}

fn expand_path(path: &str) -> String {
    if path == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
    }
    if path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{}{}", home, &path[1..]);
        }
    }
    path.to_string()
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
                self.input_mode = InputMode::Downloading;
                self.input_buffer.clear();
                self.status_msg = format!("Enter download path for '{}':", item.visible_name);
            }
        }
    }

    fn cancel_download(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.status_msg = "Download cancelled.".into();
    }

        fn confirm_download(&mut self) {
            let raw_path = self.input_buffer.trim();
            if raw_path.is_empty() {
                self.status_msg = "Path cannot be empty.".into();
                return;
            }
            let path_str = expand_path(raw_path);
    
            if let Some(i) = self.state.selected() {
                if let Some(item) = self.items.get(i) {
                    let client = self.client.clone();
                    let item_clone = item.clone(); // Clone the item
                    let name = item.visible_name.clone();
                    let tx = self.tx.clone();
                    let dest_path = path_str.clone();
    
                    self.input_mode = InputMode::Normal;
                    self.status_msg = format!("Downloading {} to {}...", name, dest_path);
                    
                    tokio::spawn(async move {
                        match download_selection(client, item_clone, dest_path).await {
                            Ok(final_path) => {
                                let _ = tx.send(AppMessage::DownloadComplete(name, final_path)).await;
                            },
                            Err(e) => {
                                let _ = tx.send(AppMessage::Error(format!("Download failed: {}", e))).await;
                            }
                        }
                    });
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
        let raw_path = self.input_buffer.trim();
        if raw_path.is_empty() {
            self.status_msg = "Path cannot be empty.".into();
            return;
        }
        let path_str = expand_path(raw_path);
        
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

    fn get_help_text(&self) -> String {
            match self.input_mode {
                InputMode::Uploading => "[Enter] Confirm Upload [Esc] Cancel".to_string(),
                InputMode::Downloading => "[Enter] Confirm Download [Esc] Cancel".to_string(),
                InputMode::Normal => {
                    let mut actions = vec!["[q] Quit", "[u] Upload", "[r] Refresh", "[j/k] Nav"];
                    
                    if !self.history.is_empty() {
                        actions.push("[h] Back");
                    }
    
                    if let Some(i) = self.state.selected() {
                        if let Some(item) = self.items.get(i) {
                            if item.is_folder() {
                                actions.push("[l/Enter] Open");
                            } else {
                                actions.push("[d] Download");
                            }
                        }
                    }
                    
                    actions.join(" | ")
                }
            }
        }}

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

fn download_recursive(client: Client, item: Item, target_path: std::path::PathBuf) -> BoxFuture<'static, Result<String>> {
    Box::pin(async move {
        if item.is_folder() {
            tokio::fs::create_dir_all(&target_path).await?;
            let children = fetch_documents(&client, &Some(item.id)).await?;
            for child in children {
                let child_name = sanitize_filename(&child.visible_name, child.is_folder());
                let child_path = target_path.join(child_name);
                download_recursive(client.clone(), child, child_path).await?;
            }
            Ok(target_path.to_string_lossy().to_string())
        } else {
            let url = format!("{}/download/{}/pdf", BASE_URL, item.id);
            let resp = client.get(&url).send().await?;
            
            // Ensure parent exists (should be handled by caller usually, but good for safety)
            if let Some(parent) = target_path.parent() {
                if !parent.exists() {
                     tokio::fs::create_dir_all(parent).await?;
                }
            }

            let mut file = tokio::fs::File::create(&target_path).await?;
            let mut stream = resp.bytes_stream();

            while let Some(chunk_res) = stream.next().await {
                let chunk = chunk_res?;
                use tokio::io::AsyncWriteExt;
                file.write_all(&chunk).await?;
            }
            Ok(target_path.to_string_lossy().to_string())
        }
    })
}

async fn download_selection(client: Client, item: Item, dest_path: String) -> Result<String> {
    let output_path = Path::new(&dest_path);
    let item_name = sanitize_filename(&item.visible_name, item.is_folder());

    // Determine final path
    let is_dir_target = dest_path.ends_with('/') || dest_path.ends_with(std::path::MAIN_SEPARATOR) || output_path.is_dir();

    let final_path = if is_dir_target {
        if !output_path.exists() {
             return Err(anyhow::anyhow!("Directory '{}' does not exist.", dest_path));
        }
        output_path.join(item_name)
    } else {
        // If user gave a file-like path for a folder download, we treat it as the folder name
        output_path.to_path_buf()
    };

    // Ensure parent dir exists
    if let Some(parent) = final_path.parent() {
        if !parent.exists() {
            return Err(anyhow::anyhow!("Directory '{}' does not exist.", parent.display()));
        }
    }

    download_recursive(client, item, final_path).await
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
                        InputMode::Downloading => match key.code {
                            KeyCode::Enter => app.confirm_download(),
                            KeyCode::Esc => app.cancel_download(),
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
                AppMessage::DownloadComplete(name, path) => {
                    app.status_msg = format!("Downloaded {} to {}.", name, path);
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
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(4),
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

    f.render_stateful_widget(items_list, main_chunks[0], &mut app.state);

    // Bottom Box (Status + Keybinds)
    let bottom_block = Block::default()
        .borders(Borders::ALL)
        .title(" Status ");
    
    let bottom_area = main_chunks[1];
    f.render_widget(bottom_block.clone(), bottom_area);

    let bottom_inner = bottom_block.inner(bottom_area);
    
    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(bottom_inner);

    // Status Bar
    let status_style = match app.input_mode {
        InputMode::Uploading | InputMode::Downloading => Style::default().bg(Color::Blue).fg(Color::White),
        InputMode::Normal => Style::default().fg(Color::White),
    };
    let status = Paragraph::new(app.status_msg.clone()).style(status_style);
    f.render_widget(status, bottom_chunks[0]);

    // Keybinds Bar
    let help_text = app.get_help_text();
    let help = Paragraph::new(help_text).style(Style::default().fg(Color::White));
    f.render_widget(help, bottom_chunks[1]);

    // Input Modal
    if let InputMode::Uploading | InputMode::Downloading = app.input_mode {
        let area = centered_rect(60, 20, f.area());
        f.render_widget(Clear, area); // Clear background

        let title = if let InputMode::Uploading = app.input_mode { " Upload File " } else { " Download File " };

        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(title)
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