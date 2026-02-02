# remarkable-tui

A lightweight Terminal User Interface (TUI) for interacting with your **reMarkable 2** tablet over its USB web interface. 

Built with Rust, [Ratatui](https://ratatui.rs/), and [Tokio](https://tokio.rs/).

## 🚀 Key Features

- **Vim-Style Navigation**: Navigate your tablet's file system effortlessly using familiar keybindings.
- **File Downloads**: Stream documents directly from your tablet and save them as PDFs locally.
- **File Uploads**: Easily upload local files to the current directory on your device.
- **Live Status Updates**: Asynchronous operations ensure the UI remains responsive, with a status bar for real-time feedback.
- **Folder Support**: Full navigation into folders and back out to root.
- **Sanitized Filenames**: Automatic sanitization of filenames during download to ensure compatibility with your local file system.

## 🛠 Tech Stack

- **Language**: Rust
- **UI Framework**: Ratatui
- **Backend**: Crossterm
- **Async Runtime**: Tokio
- **HTTP Client**: Reqwest

## 📋 Prerequisites

1.  **reMarkable 2 Tablet**: Connected to your computer via USB.
2.  **USB Web Interface**: Must be enabled on the tablet (`Settings` -> `Help` -> `Copyrights and licenses` -> `General information` -> `USB web interface`).
3.  **Connectivity**: The app expects the tablet to be reachable at `http://10.11.99.1`.

## ⚡ Quick Start

### Build and Run
```bash
# Clone the repository (if applicable)
# cd remarkable-tui

# Run the app immediately
cargo run
```

### Install via Homebrew (macOS)
```bash
# Tap the repository
brew tap crusty-crumpet-79/remarkable-tui https://github.com/crusty-crumpet-79/remarkable-tui

# Install the package
brew install remarkable-tui
```

### Install Globally (Cargo)
To use the `remarkable` command from anywhere:
```bash
cargo install --path .
```

## ⌨️ Control Scheme

| Key | Action |
|-----|--------|
| `j` / `Down` | Move selection down |
| `k` / `Up` | Move selection up |
| `l` / `Enter` | Enter directory |
| `h` / `Left` / `Backspace` | Go back/up a directory |
| `d` | Download selected file as PDF |
| `u` | Open upload modal (type local path) |
| `r` | Refresh current file list |
| `q` | Quit application |

### Input Mode (Uploading)
When the upload modal is open:
- **Type**: Enter the local path to the file you wish to upload.
- **Enter**: Confirm and start upload.
- **Esc**: Cancel upload.

## 📄 API Notes
This tool interacts with the reMarkable's built-in web server. Note that the tablet's API requires a list refresh immediately before uploading to ensure files are placed in the correct directory. This behavior is handled automatically by `remarkable-tui`.
