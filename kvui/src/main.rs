use anyhow::{anyhow, Result};
use clap::{Parser, ValueEnum};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, terminal};
use kv_core::{driver, keystore, tasks};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;
use scraper::{Html, Selector};
use std::fs::{self, File};
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;
use url::Url;

mod catalog;

#[derive(Clone, Debug)]
struct Song {
    title: String,
    artist: String,
    url: String,
    first_seen: i64,
    purchase_date: i64,
}

#[derive(Clone, Debug)]
struct Track {
    name: String,
    selected: bool,
}

#[derive(Clone, Debug)]
struct SongDetails {
    tracks: Vec<String>,
    base_key: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Menu,
    Songs,
    SongDetail,
    DownloadStatus,
    DownloadDone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SongSort {
    DateDescending,
    Artist,
    Title,
}

impl SongSort {
    fn next(self) -> Self {
        match self {
            Self::DateDescending => Self::Artist,
            Self::Artist => Self::Title,
            Self::Title => Self::DateDescending,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::DateDescending => "Recently added",
            Self::Artist => "Artist",
            Self::Title => "Title",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailFocus {
    Tracks,
    Download,
}

struct App {
    screen: Screen,
    menu_items: Vec<String>,
    menu_index: usize,
    songs: Vec<Song>,
    all_songs: Vec<Song>,
    songs_state: ListState,
    // Visual rows include blank separators, while songs_state retains real song indices.
    songs_display_state: ListState,
    song_sort: SongSort,
    song_filter: String,
    editing_song_filter: bool,
    tracks: Vec<Track>,
    tracks_state: ListState,
    detail_focus: DetailFocus,
    intro_click: bool,
    key_shift: i8,
    base_key: Option<String>,
    status: String,
    current_song: Option<Song>,
    download_dir: PathBuf,
    download: Option<DownloadState>,
    logs: Arc<Mutex<LogBuffer>>,
    log_scroll: u16,
    log_follow: bool,
    loading: Option<LoadingState>,
    spinner_index: usize,
    confirm: Option<ConfirmState>,
    incomplete_folders: Vec<PathBuf>,
    recovery_notice_open: bool,
    recovery_selected: usize,
    recovery_details_open: bool,
    recovery_cleanup_confirm_open: bool,
    recovery_cleanup_selected: usize,
    recovery_detail_scroll: usize,
    done_menu_items: Vec<String>,
    done_menu_index: usize,
}

impl App {
    fn new(logs: Arc<Mutex<LogBuffer>>) -> Self {
        let mut app = Self {
            screen: Screen::Menu,
            menu_items: Vec::new(),
            menu_index: 0,
            songs: Vec::new(),
            all_songs: Vec::new(),
            songs_state: ListState::default(),
            songs_display_state: ListState::default(),
            song_sort: SongSort::DateDescending,
            song_filter: String::new(),
            editing_song_filter: false,
            tracks: Vec::new(),
            tracks_state: ListState::default(),
            detail_focus: DetailFocus::Tracks,
            intro_click: false,
            key_shift: 0,
            base_key: None,
            status: String::new(),
            current_song: None,
            download_dir: load_download_dir()
                .unwrap_or_else(|_| default_download_dir().unwrap_or_else(|| PathBuf::from("."))),
            download: None,
            logs,
            log_scroll: 0,
            log_follow: true,
            loading: None,
            spinner_index: 0,
            confirm: None,
            incomplete_folders: Vec::new(),
            recovery_notice_open: false,
            recovery_selected: 1,
            recovery_details_open: false,
            recovery_cleanup_confirm_open: false,
            recovery_cleanup_selected: 1,
            recovery_detail_scroll: 0,
            done_menu_items: vec![
                "Open in File Explorer".to_string(),
                "Back to Songs".to_string(),
                "Quit".to_string(),
            ],
            done_menu_index: 0,
        };
        app.refresh_menu();
        app
    }

    /// Identify incomplete downloads without modifying files.
    fn scan_incomplete_downloads(&mut self) {
        self.incomplete_folders.clear();

        if !self.download_dir.is_dir() {
            return;
        }

        // Clean abandoned staging folders before checking incomplete downloads.
        if let Err(err) = cleanup_abandoned_staging(&self.download_dir) {
            tracing::warn!("Pre-scan staging cleanup failed: {}", err);
        }

        match tasks::download_song::list_kvui_staging_directories(
            &self.download_dir,
        ) {
            Ok(folders) => {
                for folder in folders {
                    match tasks::download_song::kvui_staging_activity(&folder) {
                        Ok(tasks::download_song::KvuiStagingActivity::Active) => {}
                        Ok(tasks::download_song::KvuiStagingActivity::Abandoned)
                        | Ok(tasks::download_song::KvuiStagingActivity::Legacy) => {
                            self.incomplete_folders.push(folder);
                        }
                        Err(err) => {
                            tracing::warn!(
                                "Could not inspect staging folder {}: {}",
                                folder.display(),
                                err
                            );
                        }
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    "Could not scan download folder {}: {}",
                    self.download_dir.display(),
                    err
                );
            }
        }

        tracing::info!(
            "Incomplete download folders found: {}",
            self.incomplete_folders.len()
        );
    }
    fn refresh_menu(&mut self) {
        let authenticated = is_authenticated();
        self.menu_items = if authenticated {
            vec![
                "Browse My Songs".to_string(),
                "Resync Songs List".to_string(),
                "Set Download Folder".to_string(),
                "Logout".to_string(),
                "Quit".to_string(),
            ]
        } else {
            vec![
                "Browse My Songs".to_string(),
                "Resync Songs List".to_string(),
                "Set Download Folder".to_string(),
                "Login".to_string(),
                "Quit".to_string(),
            ]
        };
        if !self.incomplete_folders.is_empty() {
            let label = format!(
                "Incomplete Downloads ({})",
                self.incomplete_folders.len()
            );

            let position = self.menu_items
                .iter()
                .position(|item| item == "Quit")
                .unwrap_or(self.menu_items.len());

            self.menu_items.insert(position, label);
        }
        self.menu_index = 0;
    }
}

fn demo_songs() -> Vec<Song> {
    vec![
        Song {
            title: "Don't You (Forget About Me)".to_string(),
            artist: "Simple Minds".to_string(),
            url: "https://example.com/simple-minds".to_string(),
            first_seen: 5,
            purchase_date: 20260206,
        },
        Song {
            title: "What I Like About You".to_string(),
            artist: "The Romantics".to_string(),
            url: "https://example.com/the-romantics".to_string(),
            first_seen: 4,
            purchase_date: 20251117,
        },
        Song {
            title: "Don't Look Back in Anger".to_string(),
            artist: "Oasis".to_string(),
            url: "https://example.com/oasis".to_string(),
            first_seen: 3,
            purchase_date: 20251127,
        },
        Song {
            title: "Livin' on a Prayer".to_string(),
            artist: "Bon Jovi".to_string(),
            url: "https://example.com/bon-jovi".to_string(),
            first_seen: 2,
            purchase_date: 20250831,
        },
        Song {
            title: "Use Somebody".to_string(),
            artist: "Kings of Leon".to_string(),
            url: "https://example.com/kings-of-leon".to_string(),
            first_seen: 1,
            purchase_date: 20250831,
        },
    ]
}

fn demo_tracks() -> Vec<String> {
    [
        "Click",
        "Drum Kit",
        "Bass",
        "Electric Guitar (clean)",
        "Electric Guitar (crunch)",
        "Piano",
        "Synth Pad",
        "Backing Vocals",
        "Lead Vocal",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn configure_demo_screen(app: &mut App, screen: DemoScreen) {
    app.menu_items = vec![
        "Browse My Songs".to_string(),
        "Resync Songs List".to_string(),
        "Set Download Folder".to_string(),
        "Logout".to_string(),
        "Quit".to_string(),
    ];
    app.status = "Ready".to_string();

    match screen {
        DemoScreen::Menu => app.screen = Screen::Menu,
        DemoScreen::Songs => {
            app.all_songs = demo_songs();
            apply_song_view(app);
            app.screen = Screen::Songs;
        }
        DemoScreen::Song => {
            let song = demo_songs().remove(0);
            app.current_song = Some(song);
            app.tracks = demo_tracks()
                .into_iter()
                .map(|name| Track {
                    name,
                    selected: true,
                })
                .collect();
            app.tracks_state.select(Some(1));
            app.base_key = Some("E".to_string());
            app.intro_click = false;
            app.detail_focus = DetailFocus::Tracks;
            app.screen = Screen::SongDetail;
        }
        DemoScreen::Download | DemoScreen::Complete => {
            let song = demo_songs().remove(0);
            let statuses = [
                TrackStatus::Done,
                TrackStatus::Done,
                TrackStatus::Done,
                TrackStatus::Done,
                TrackStatus::Done,
                TrackStatus::Downloading,
                TrackStatus::Pending,
                TrackStatus::Failed,
                TrackStatus::Pending,
            ];
            let tracks = demo_tracks()
                .into_iter()
                .zip(statuses)
                .map(|(name, status)| TrackStatusItem {
                    name,
                    attempt: if status == TrackStatus::Downloading { 2 } else { 0 },
                    status,
                })
                .collect();
            let complete = matches!(screen, DemoScreen::Complete);
            app.download = Some(DownloadState {
                song,
                count_in: false,
                transpose: 0,
                tracks,
                handle: None,
                published_rx: None,
                done: complete.then_some(Err(
                    "1 track failed after retries; completed tracks were saved.".to_string(),
                )),
                started_at: Instant::now(),
                duration: complete.then_some(Duration::from_secs(154)),
                download_dir: dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("~"))
                    .join("Downloads"),
            });
            if complete {
                if let Some(download) = app.download.as_mut() {
                    for track in &mut download.tracks {
                        if track.status == TrackStatus::Downloading
                            || track.status == TrackStatus::Pending
                        {
                            track.status = TrackStatus::Done;
                        }
                    }
                }
                app.screen = Screen::DownloadDone;
            } else {
                if let Ok(mut logs) = app.logs.lock() {
                    logs.push_line("INFO  Processing track 7 'Piano'".to_string());
                    logs.push_line("INFO  Attempt 2/3 for 'Piano'".to_string());
                    logs.push_line("INFO  Waiting for 'Piano' to be prepared".to_string());
                }
                app.screen = Screen::DownloadStatus;
            }
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "kvui", version, about = "KV Downloader terminal UI")]
struct Cli {
    #[arg(
        long,
        value_enum,
        hide = true,
        help = "Render a deterministic screen for documentation screenshots"
    )]
    demo_screen: Option<DemoScreen>,

    #[arg(
        long,
        value_name = "PATH",
        num_args = 0..=1,
        default_missing_value = "kvui-diagnostics.log",
        help = "Write detailed diagnostics to a log file"
    )]
    diagnostics: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DemoScreen {
    Menu,
    Songs,
    Song,
    Download,
    Complete,
}

fn main() -> Result<()> {
    dotenv::dotenv().ok();
    let cli = Cli::parse();
    run_tui(cli.diagnostics, cli.demo_screen)
}

fn run_tui(diagnostics_path: Option<PathBuf>, demo_screen: Option<DemoScreen>) -> Result<()> {
    let logs = Arc::new(Mutex::new(LogBuffer::new(1000)));
    let diagnostics = match diagnostics_path.as_deref() {
        Some(path) => Some(Arc::new(Mutex::new(File::create(path)?))),
        None => None,
    };
    setup_tracing(logs.clone(), diagnostics);
    if let Some(path) = diagnostics_path {
        tracing::info!("Writing detailed diagnostics to {}", path.display());
    }

    let mut terminal = setup_terminal()?;
    let tick_rate = Duration::from_millis(80);
    let mut last_tick = Instant::now();
    let mut app = App::new(logs);
    if demo_screen.is_none() {
        app.scan_incomplete_downloads();
        app.refresh_menu();
        app.recovery_notice_open = !app.incomplete_folders.is_empty();
    }
    if let Some(screen) = demo_screen {
        configure_demo_screen(&mut app, screen);
    }

    loop {
        if demo_screen.is_none() {
            update_download_state(&mut app);
            update_loading_state(&mut app);
        }
        terminal.draw(|f| ui(f, &mut app))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press
                    && handle_key_event(&mut terminal, &mut app, key)?
                {
                    break;
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            if app.loading.is_some() {
                app.spinner_index = app.spinner_index.wrapping_add(1);
            }
            last_tick = Instant::now();
        }
    }

    if demo_screen.is_none() && app.download_dir.is_dir() {
        if let Err(err) = cleanup_abandoned_staging(&app.download_dir) {
            tracing::warn!("Shutdown staging cleanup failed: {}", err);
        }
    }
    restore_terminal(&mut terminal)?;
    Ok(())
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, terminal::DisableLineWrap)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        terminal::EnableLineWrap
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn handle_key_event(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    key: KeyEvent,
) -> Result<bool> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(true);
    }
    if app.recovery_notice_open {
        return handle_recovery_keys(app, key);
    }
    if app.confirm.is_some() {
        return handle_confirm_keys(app, key);
    }

    match app.screen {
        Screen::Menu => handle_menu_keys(terminal, app, key),
        Screen::Songs => handle_songs_keys(terminal, app, key),
        Screen::SongDetail => handle_detail_keys(terminal, app, key),
        Screen::DownloadStatus => handle_download_keys(app, key),
        Screen::DownloadDone => handle_done_keys(app, key),
    }
}

fn handle_menu_keys(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    key: KeyEvent,
) -> Result<bool> {
    match key.code {
        KeyCode::Esc => {},
        KeyCode::Up => {
            if app.menu_index > 0 {
                app.menu_index -= 1;
            }
        }
        KeyCode::Down => {
            if app.menu_index + 1 < app.menu_items.len() {
                app.menu_index += 1;
            }
        }
        KeyCode::Enter => {
            let selected = app
                .menu_items
                .get(app.menu_index)
                .cloned()
                .unwrap_or_default();
            match selected.as_str() {
                item if item.starts_with("Incomplete Downloads (") => {
                    app.scan_incomplete_downloads();
                    app.refresh_menu();

                    if app.incomplete_folders.is_empty() {
                        app.status =
                            "No incomplete downloads remain.".to_string();
                    } else {
                        app.recovery_selected = 1;
                        app.recovery_details_open = false;
                        app.recovery_detail_scroll = 0;
                        app.recovery_cleanup_confirm_open = false;
                        app.recovery_cleanup_selected = 1;
                        app.recovery_notice_open = true;
                    }
                }
                "Login" => {
                    app.status = "".to_string();
                    restore_terminal(terminal)?;
                    let login_result = login_flow();
                    *terminal = setup_terminal()?;
                    match login_result {
                        Ok(_) => {
                            app.status = "Login complete".to_string();
                            app.refresh_menu();
                        }
                        Err(err) => {
                            app.status = format!("Login failed: {}", err);
                        }
                    }
                }
                "Logout" => {
                    keystore::Keystore::logout()?;
                    keystore::Keystore::clear_auth_cookie()?;
                    catalog::SongCatalog::open()?.clear()?;
                    app.songs.clear();
                    app.all_songs.clear();
                    app.song_filter.clear();
                    app.status = "Logged out; local catalog cleared".to_string();
                    app.refresh_menu();
                }
                "Browse My Songs" => {
                    app.status = "".to_string();
                    let cached = catalog::SongCatalog::open()?.list()?;
                    if cached.is_empty() {
                        match cookie_value() {
                            Ok(cookie) => start_loading_songs(app, cookie, false),
                            Err(err) => app.status = format!("Not authenticated: {}", err),
                        }
                    } else {
                        set_song_list(app, cached);
                        if let Ok(cookie) = cookie_value() {
                            start_loading_songs(app, cookie, false);
                        }
                    }
                }
                "Resync Songs List" => {
                    app.status = "".to_string();
                    match cookie_value() {
                        Ok(cookie) => start_loading_songs(app, cookie, true),
                        Err(err) => app.status = format!("Not authenticated: {}", err),
                    }
                }
                "Set Download Folder" => {
                    restore_terminal(terminal)?;
                    let selection = prompt_download_folder(&app.download_dir);
                    *terminal = setup_terminal()?;
                    match selection {
                        Ok(Some(path)) => match save_download_dir(&path) {
                            Ok(()) => {
                                app.download_dir = path;
                                app.status = format!(
                                    "Download folder set to {}",
                                    app.download_dir.display()
                                );
                            }
                            Err(err) => {
                                app.status = format!("Could not save download folder: {err}");
                            }
                        },
                        Ok(None) => {
                            app.status = format!(
                                "Download folder unchanged: {}",
                                app.download_dir.display()
                            );
                        }
                        Err(err) => app.status = format!("Invalid download folder: {err}"),
                    }
                }
                "Quit" => return Ok(true),
                _ => {}
            }
        }
        KeyCode::Char('q') => return Ok(true),
        _ => {}
    }

    Ok(false)
}

fn set_song_list(app: &mut App, songs: Vec<Song>) {
    app.all_songs = songs;
    apply_song_view(app);
    app.screen = Screen::Songs;
}

fn apply_song_view(app: &mut App) {
    let filter = app.song_filter.to_lowercase();
    app.songs = app
        .all_songs
        .iter()
        .filter(|song| {
            filter.is_empty()
                || song.title.to_lowercase().contains(&filter)
                || song.artist.to_lowercase().contains(&filter)
        })
        .cloned()
        .collect();
    match app.song_sort {
        SongSort::DateDescending => app.songs.sort_by(|left, right| {
            right
                .purchase_date
                .cmp(&left.purchase_date)
                .then_with(|| right.first_seen.cmp(&left.first_seen))
                .then_with(|| left.artist.to_lowercase().cmp(&right.artist.to_lowercase()))
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
        }),
        SongSort::Artist => app.songs.sort_by(|left, right| {
            left.artist
                .to_lowercase()
                .cmp(&right.artist.to_lowercase())
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
        }),
        SongSort::Title => app
            .songs
            .sort_by_key(|song| (song.title.to_lowercase(), song.artist.to_lowercase())),
    }
    app.songs_state = ListState::default();
    if !app.songs.is_empty() {
        app.songs_state.select(Some(0));
    }
}

/// Cycle the sort order while retaining the same highlighted song when present.
fn cycle_song_sort_preserving_selection(app: &mut App) {
    let selected_url = app.songs_state.selected()
        .and_then(|index| app.songs.get(index))
        .map(|song| song.url.clone());

    app.song_sort = app.song_sort.next();
    apply_song_view(app);

    if let Some(url) = selected_url {
        if let Some(index) = app.songs.iter().position(|song| song.url == url) {
            app.songs_state.select(Some(index));
        }
    }
}

fn handle_songs_keys(
    _terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    key: KeyEvent,
) -> Result<bool> {
    if app.editing_song_filter {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => app.editing_song_filter = false,
            KeyCode::Backspace => {
                app.song_filter.pop();
                apply_song_view(app);
            }
            KeyCode::Char(character) => {
                app.song_filter.push(character);
                apply_song_view(app);
            }
            _ => {}
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Esc => {
            app.screen = Screen::Menu;
            app.status = "".to_string();
        }
        KeyCode::Up => {
            let next = app
                .songs_state
                .selected()
                .and_then(|idx| idx.checked_sub(1))
                .unwrap_or(0);
            if !app.songs.is_empty() {
                app.songs_state.select(Some(next));
            }
        }
        KeyCode::Down => {
            let selected = app.songs_state.selected().unwrap_or(0);
            let next = if selected + 1 >= app.songs.len() {
                selected
            } else {
                selected + 1
            };
            if !app.songs.is_empty() {
                app.songs_state.select(Some(next));
            }
        }
        KeyCode::Char('s') | KeyCode::Char('S') => {
            cycle_song_sort_preserving_selection(app);
        }
        KeyCode::Enter => {
            if let Some(idx) = app.songs_state.selected() {
                if let Some(song) = app.songs.get(idx).cloned() {
                    app.status = "".to_string();
                    match cookie_value() {
                        Ok(cookie) => start_loading_tracks(app, cookie, song),
                        Err(err) => app.status = format!("Not authenticated: {}", err),
                    }
                }
            }
        }
        _ => {}
    }

    Ok(false)
}

/// Change all track checkboxes only while the track pane has focus.
fn bulk_select_tracks(app: &mut App, selected: bool) {
    if app.detail_focus == DetailFocus::Tracks {
        for track in &mut app.tracks {
            track.selected = selected;
        }
    }
}

fn back_from_song_details(app: &mut App) {
    app.screen = Screen::Songs;
    app.status.clear();
}

fn handle_detail_keys(
    _terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    key: KeyEvent,
) -> Result<bool> {
    if app.confirm.is_some() {
        return handle_confirm_keys(app, key);
    }

    match key.code {
        KeyCode::Esc => {
            back_from_song_details(app);
        }
        KeyCode::Tab => {
            app.detail_focus = match app.detail_focus {
                DetailFocus::Tracks => DetailFocus::Download,
                DetailFocus::Download => DetailFocus::Tracks,
            };
        }
        KeyCode::Up => {
            if app.detail_focus == DetailFocus::Tracks {
                let next = app
                    .tracks_state
                    .selected()
                    .and_then(|idx| idx.checked_sub(1))
                    .unwrap_or(0);
                if !app.tracks.is_empty() {
                    app.tracks_state.select(Some(next));
                }
            }
        }
        KeyCode::Down => {
            if app.detail_focus == DetailFocus::Tracks {
                let selected = app.tracks_state.selected().unwrap_or(0);
                let next = if selected + 1 >= app.tracks.len() {
                    selected
                } else {
                    selected + 1
                };
                if !app.tracks.is_empty() {
                    app.tracks_state.select(Some(next));
                }
            }
        }
        KeyCode::Char(' ') => {
            if app.detail_focus == DetailFocus::Tracks {
                if let Some(idx) = app.tracks_state.selected() {
                    if let Some(track) = app.tracks.get_mut(idx) {
                        track.selected = !track.selected;
                    }
                }
            }
        }
        KeyCode::Char('a') | KeyCode::Char('A') => bulk_select_tracks(app, true),
        KeyCode::Char('n') | KeyCode::Char('N') => bulk_select_tracks(app, false),
        KeyCode::Char('i') => {
            app.intro_click = !app.intro_click;
        }
        KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Right => {
            if app.key_shift < 4 {
                app.key_shift += 1;
            }
        }
        KeyCode::Char('-') | KeyCode::Left => {
            if app.key_shift > -4 {
                app.key_shift -= 1;
            }
        }
        KeyCode::Enter => {
            if app.detail_focus == DetailFocus::Download {
                if let Some(song) = app.current_song.clone() {
                    app.status = "Starting download...".to_string();
                    let selected_tracks: Vec<String> = app
                        .tracks
                        .iter()
                        .filter(|t| t.selected)
                        .map(|t| t.name.clone())
                        .collect();
                    if selected_tracks.is_empty() {
                        app.status = "Select at least one track to download".to_string();
                        return Ok(false);
                    }
                    app.confirm = Some(ConfirmState {
                        message: vec![
                            "This will use the verified HTTP downloader.".to_string(),
                            "Selected stems will safely replace existing files.".to_string(),
                            "Choose Download Process".to_string(),
                        ],
                        selected: 0,
                        payload: ConfirmPayload::StartDownload {
                            song,
                            selected_tracks,
                            intro_click: app.intro_click,
                            key_shift: app.key_shift,
                        },
                    });
                }
            }
        }
        _ => {}
    }

    Ok(false)
}

#[cfg(test)]
mod c24_navigation_selection_tests {
    use super::*;

    fn test_app() -> App {
        App::new(Arc::new(Mutex::new(LogBuffer::new(20))))
    }

    #[test]
    fn http_publication_event_overrides_log_based_pending_state() {
        let mut app = test_app();
        configure_demo_screen(&mut app, DemoScreen::Download);
        let (tx, rx) = std::sync::mpsc::channel();
        app.download.as_mut().unwrap().published_rx = Some(rx);
        // Piano is initially Downloading. A publication event is authoritative.
        tx.send("Piano".to_string()).unwrap();
        update_download_state(&mut app);
        assert_eq!(app.download.as_ref().unwrap().tracks[5].status, TrackStatus::Done);
    }

    #[test]
    fn select_all_and_none_only_change_tracks_in_track_pane() {
        let mut app = test_app();
        app.tracks = vec![
            Track { name: "Drum Kit".into(), selected: true },
            Track { name: "Bass".into(), selected: false },
            Track { name: "Piano".into(), selected: true },
        ];
        app.detail_focus = DetailFocus::Tracks;
        bulk_select_tracks(&mut app, false);
        assert!(app.tracks.iter().all(|track| !track.selected));
        bulk_select_tracks(&mut app, true);
        assert!(app.tracks.iter().all(|track| track.selected));

        app.detail_focus = DetailFocus::Download;
        bulk_select_tracks(&mut app, false);
        assert!(app.tracks.iter().all(|track| track.selected));
    }

    #[test]
    fn sorting_retains_highlighted_song_through_each_sort_order() {
        let mut app = test_app();
        app.all_songs = vec![
            Song { artist: "Zulu".into(), title: "Alpha".into(), url: "https://example.test/one".into(), first_seen: 3, purchase_date: 20260101 },
            Song { artist: "Beta".into(), title: "Charlie".into(), url: "https://example.test/two".into(), first_seen: 2, purchase_date: 20260201 },
            Song { artist: "Alpha".into(), title: "Bravo".into(), url: "https://example.test/three".into(), first_seen: 1, purchase_date: 20260301 },
        ];
        apply_song_view(&mut app);
        let wanted = "https://example.test/one";
        let first_index = app.songs.iter().position(|song| song.url == wanted).unwrap();
        app.songs_state.select(Some(first_index));

        for expected in [SongSort::Artist, SongSort::Title, SongSort::DateDescending] {
            cycle_song_sort_preserving_selection(&mut app);
            assert_eq!(app.song_sort, expected);
            let selected = app.songs_state.selected().unwrap();
            assert_eq!(app.songs[selected].url, wanted);
        }
    }

    #[test]
    fn detail_back_returns_to_existing_song_list() {
        let mut app = test_app();
        app.screen = Screen::SongDetail;
        app.status = "Temporary message".into();
        app.songs_state.select(Some(2));
        back_from_song_details(&mut app);
        assert_eq!(app.screen, Screen::Songs);
        assert_eq!(app.songs_state.selected(), Some(2));
        assert!(app.status.is_empty());
    }
}

fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    match app.screen {
        Screen::Menu => render_menu(frame, app),
        Screen::Songs => render_songs(frame, app),
        Screen::SongDetail => render_detail(frame, app),
        Screen::DownloadStatus => render_download_status(frame, app),
        Screen::DownloadDone => render_download_done(frame, app),
    }
    if app.loading.is_some() {
        render_loading_overlay(frame, app);
    }
    if app.confirm.is_some() {
        render_confirm_overlay(frame, app);
    }
    if app.recovery_notice_open {
        render_recovery_overlay(frame, app);
    }
}

/// Ordinary text is soft gray rather than terminal-default white.
fn normal_text_style() -> Style {
    Style::default().fg(Color::Gray)
}

/// Match the Main Menu's gold text selection; no light background fill.
fn selected_text_style() -> Style {
    Style::default().fg(Color::Yellow)
}

/// Unchecked stems are dimmer than checked stems; the keyboard cursor
/// remains gold so its position is always visible, even on an unchecked row.
fn track_name_style(is_highlighted: bool, selected: bool) -> Style {
    if is_highlighted {
        selected_text_style()
    } else if selected {
        normal_text_style()
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn render_menu(frame: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(frame.size());

    let logo = vec![
        Line::from(Span::styled(
            r"██╗  ██╗██╗   ██╗      ██████╗  ██████╗ ██╗    ██╗███╗   ██╗██╗      ██████╗  █████╗ ██████╗ ███████╗██████╗ ",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            r"██║ ██╔╝██║   ██║      ██╔══██╗██╔═══██╗██║    ██║████╗  ██║██║     ██╔═══██╗██╔══██╗██╔══██╗██╔════╝██╔══██╗",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            r"█████╔╝ ██║   ██║█████╗██║  ██║██║   ██║██║ █╗ ██║██╔██╗ ██║██║     ██║   ██║███████║██║  ██║█████╗  ██████╔╝",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            r"██╔═██╗ ╚██╗ ██╔╝╚════╝██║  ██║██║   ██║██║███╗██║██║╚██╗██║██║     ██║   ██║██╔══██║██║  ██║██╔══╝  ██╔══██╗",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            r"██║  ██╗ ╚████╔╝       ██████╔╝╚██████╔╝╚███╔███╔╝██║ ╚████║███████╗╚██████╔╝██║  ██║██████╔╝███████╗██║  ██║",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            r"╚═╝  ╚═╝  ╚═══╝        ╚═════╝  ╚═════╝  ╚══╝╚══╝ ╚═╝  ╚═══╝╚══════╝ ╚═════╝ ╚═╝  ╚═╝╚═════╝ ╚══════╝╚═╝  ╚═╝",
            Style::default().fg(Color::Cyan),
        )),
    ];
    let logo = Paragraph::new(Text::from(logo))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("KV Downloader")
                .border_style(Style::default().fg(Color::Cyan)),
        );
    frame.render_widget(logo, chunks[0]);

    let items: Vec<ListItem> = app
        .menu_items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let prefix = if i == app.menu_index { ">" } else { " " };
            let style = if i == app.menu_index {
                selected_text_style()
            } else {
                normal_text_style()
            };
            if item == "Set Download Folder" {
                let current = abbreviate_path(&app.download_dir, 20);
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{} {}", prefix, item), style),
                    Span::styled(
                        format!("  (current: {current})"),
                        normal_text_style(),
                    ),
                ]))
            } else {
                ListItem::new(Line::from(Span::styled(
                    format!("{} {}", prefix, item),
                    style,
                )))
            }
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title("Main Menu")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(Style::default());

    frame.render_widget(list, chunks[1]);

    let status = Paragraph::new(app.status.clone())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Status")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .style(normal_text_style())
        .wrap(Wrap { trim: true });
    frame.render_widget(status, chunks[2]);

    frame.render_widget(footer_help("Enter: select | ESC: back | Ctrl+C: quit"), chunks[3]);
}

/// Render lower instruction bars with one consistent readable style.
/// The leading two spaces separate instructions from the left border.
fn footer_help(text: &str) -> Paragraph<'static> {
    Paragraph::new(format!("  {text}"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Left)
}

/// Fit one label to a terminal column without moving the next column.
fn clip_song_column(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut clipped = value.chars().take(width - 1).collect::<String>();
    clipped.push('…');
    clipped
}

/// Artist is the first column only when sorting by artist.
/// Dot leaders give every second column a consistent starting position.
fn song_list_row(song: &Song, sort: SongSort, first_width: usize, total_width: usize) -> String {
    let (first, second) = if sort == SongSort::Artist {
        (&song.artist, &song.title)
    } else {
        (&song.title, &song.artist)
    };
    let first = clip_song_column(first, first_width.saturating_sub(3));
    let leader = ".".repeat(first_width.saturating_sub(first.chars().count()));
    let second = clip_song_column(second, total_width.saturating_sub(first_width));
    format!("{first}{leader}{second}")
}

/// Blank items before each real song create whitespace without changing the
/// logical song indices or allowing blank rows to receive the highlight.
fn song_list_items(songs: &[Song], sort: SongSort, total_width: usize) -> Vec<ListItem<'static>> {
    let longest_first = songs.iter()
        .map(|song| if sort == SongSort::Artist { &song.artist } else { &song.title })
        .map(|label| label.chars().count())
        .max()
        .unwrap_or(0);
    // Leave at least 18 columns for the right-hand text in an ordinary terminal.
    let first_width = longest_first.saturating_add(4)
        .max(24)
        .min(50)
        .min(total_width.saturating_sub(18));
    let mut items = Vec::with_capacity(songs.len() * 2);
    for song in songs {
        items.push(ListItem::new(Line::default()));
        items.push(ListItem::new(Line::from(song_list_row(
            song, sort, first_width, total_width,
        ))));
    }
    items
}

fn song_display_index(index: usize) -> usize {
    index.saturating_mul(2).saturating_add(1)
}

fn render_songs(frame: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)].as_ref())
        .split(frame.size());

    let available_width = chunks[0].width.saturating_sub(2) as usize;
    let items = song_list_items(&app.songs, app.song_sort, available_width);

    let base_title = "My Songs";
    let filter = if app.song_filter.is_empty() {
        String::new()
    } else {
        format!(" | Filter: {}", app.song_filter)
    };
    let editing = if app.editing_song_filter { " [typing]" } else { "" };
    let title = format!(
        "{} | SORT [S]: {}{}{}",
        base_title,
        app.song_sort.label(),
        filter,
        editing
    );
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .style(normal_text_style())
        .highlight_style(selected_text_style());

    // The display list uses [blank, song, blank, song, ...]. Keep the
    // persistent navigation state expressed as real song indices.
    let display_selection = app.songs_state.selected().map(song_display_index);
    app.songs_display_state.select(display_selection);
    frame.render_stateful_widget(list, chunks[0], &mut app.songs_display_state);

    let help_text = "S: CHANGE SORT (Recent / Artist / Title) | Enter: open | Esc: back";
    frame.render_widget(footer_help(help_text), chunks[1]);
}
fn render_detail(frame: &mut ratatui::Frame, app: &mut App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(5),
            Constraint::Length(5),
            Constraint::Length(3),
        ])
        .split(frame.size());

    let title = app
        .current_song
        .as_ref()
        .map(|s| format!("{} - {}", s.title, s.artist))
        .unwrap_or_else(|| "Song".to_string());

    let key_display = format_key_display(app.base_key.as_deref(), app.key_shift);
    let controls = vec![
        Line::from(vec![
            Span::styled("Intro Click: ", normal_text_style()),
            Span::raw(if app.intro_click { "On" } else { "Off" }),
            Span::raw("   "),
            Span::styled("Key: ", normal_text_style()),
            Span::raw(key_display),
            Span::raw("   Method: "),
            Span::raw("HTTP"),
        ]),
        Line::from("i: Intro Click   +/-: Key"),
        Line::from(vec![
            Span::styled(
                "Download folder: ",
                normal_text_style(),
            ),
            Span::raw(app.download_dir.display().to_string()),
        ]),
    ];

    let header = Paragraph::new(Text::from(controls))
        .style(normal_text_style())
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(header, layout[0]);

    let items: Vec<ListItem> = app
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let is_highlighted = app.detail_focus == DetailFocus::Tracks
                && app.tracks_state.selected() == Some(index);
            let text_style = track_name_style(is_highlighted, track.selected);
            let bracket_style = text_style;
            let mark_style = if is_highlighted {
                selected_text_style()
            } else if track.selected {
                Style::default().fg(Color::Green)
            } else {
                track_name_style(false, false)
            };
            let line = Line::from(vec![
                Span::styled("[", bracket_style),
                Span::styled(if track.selected { "x" } else { " " }, mark_style),
                Span::styled("] ", bracket_style),
                Span::styled(track.name.clone(), text_style),
            ]);
            ListItem::new(line)
        })
        .collect();

    let selected_count = app.tracks
        .iter()
        .filter(|track| track.selected)
        .count();

    let track_count = format!(
        "{} of {} tracks selected",
        selected_count,
        app.tracks.len()
    );

    let track_block_title = if app.detail_focus == DetailFocus::Tracks {
        format!("Tracks (Space: toggle, A: all, N: none) | {track_count}")
    } else {
        format!("Tracks | {track_count}")
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(track_block_title)
                .borders(Borders::ALL)
                .border_style(if app.detail_focus == DetailFocus::Tracks {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                }),
        )
        // Selection color is applied explicitly to the selected row above,
        // preserving it only when the Tracks pane has focus.
        .style(normal_text_style());

    frame.render_stateful_widget(list, layout[1], &mut app.tracks_state);

    render_download_block(
        frame,
        layout[2],
        app.detail_focus == DetailFocus::Download,
        &app.status,
        &app.download_dir,
    );

    frame.render_widget(
        footer_help("Space: toggle | A: all | N: none | Tab: panes | Esc: songs"),
        layout[3],
    );
}

fn render_download_status(frame: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(9)].as_ref())
        .split(frame.size());

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
        .split(chunks[0]);

    let mut title = "Download Status".to_string();
    if let Some(download) = &app.download {
        title = format!("Download Status - {} - {}", download.song.title, download.song.artist);
    }

    let track_items = if let Some(download) = &app.download {
        download
            .tracks
            .iter()
            .map(|track| {
                let marker = match track.status {
                    TrackStatus::Pending => "...",
                    TrackStatus::Downloading => ">>",
                    TrackStatus::Done => "OK",
                    TrackStatus::Failed => "!!",
                };
                let style = match track.status {
                    TrackStatus::Downloading => Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow),
                    TrackStatus::Done => Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green),
                    TrackStatus::Failed => Style::default()
                        .fg(Color::Gray)
                        .bg(Color::Red),
                    TrackStatus::Pending => normal_text_style(),
                };
                let attempt = match track.status {
                    TrackStatus::Downloading => format!(" (attempt {}/3)", track.attempt.max(1)),
                    TrackStatus::Failed => " (failed after 3 attempts)".to_string(),
                    _ => String::new(),
                };
                ListItem::new(Line::from(Span::styled(
                    format!("{} {}{}", marker, track.name, attempt),
                    style,
                )))
            })
            .collect::<Vec<_>>()
    } else {
        vec![ListItem::new("No download in progress.")]
    };

    let track_list = List::new(track_items)
        .style(normal_text_style())
        .block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(track_list, columns[0]);

    let logs = app
        .logs
        .lock()
        .ok()
        .map(|buf| buf.lines.clone())
        .unwrap_or_default();
    let visible_lines = columns[1].height.saturating_sub(2) as usize;
    let max_scroll = logs
        .len()
        .saturating_sub(visible_lines)
        .min(u16::MAX as usize) as u16;
    if app.log_follow {
        app.log_scroll = max_scroll;
    } else if app.log_scroll > max_scroll {
        app.log_scroll = max_scroll;
    }
    let log_text = logs.iter().map(|line| parse_ansi_line(line)).collect::<Vec<_>>();
    let log_block = Paragraph::new(Text::from(log_text))
        .block(
            Block::default()
                .title("Logs")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.log_scroll, 0));
    frame.render_widget(log_block, columns[1]);

    frame.render_widget(
        footer_help("Up/Down: scroll logs | End: follow latest | ESC: menu (after finish)"),
        chunks[1],
    );
}

fn count_current_download_results(tracks: &[TrackStatusItem]) -> (usize, usize, usize) {
    let completed = tracks.iter().filter(|track| track.status == TrackStatus::Done).count();
    let failed = tracks.iter().filter(|track| track.status == TrackStatus::Failed).count();
    let pending = tracks.len() - completed - failed;
    (completed, failed, pending)
}

fn render_download_done(frame: &mut ratatui::Frame, app: &mut App) {
    let Some(download) = &app.download else {
        return;
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(7)].as_ref())
        .split(frame.size());

    let (heading, heading_color) = match download.done.as_ref() {
        Some(Ok(())) => ("Download complete", Color::Green),
        Some(Err(_)) => ("Download finished with errors", Color::Red),
        None => ("Download finished", Color::Yellow),
    };
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        heading,
        Style::default()
            .fg(heading_color),
    )));
    if let Some(Err(error)) = download.done.as_ref() {
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "Song: {} - {}",
        download.song.title, download.song.artist
    )));
    if let Some(duration) = download.duration {
        lines.push(Line::from(format!(
            "Duration: {}",
            format_duration(duration)
        )));
    }
    lines.push(Line::from(format!(
        "Location: {}",
        download.download_dir.display()
    )));
    lines.push(Line::from(""));
    // Counts describe the current operation only, never historical records.
    let (completed, failed, pending) = count_current_download_results(&download.tracks);
    lines.push(Line::from(format!(
        "Tracks: {} completed, {} failed, {} pending", completed, failed, pending
    )));
    for track in &download.tracks {
        let marker = match track.status {
            TrackStatus::Done => "✓",
            TrackStatus::Failed => "✗",
            _ => "-",
        };
        lines.push(Line::from(format!("  {} {}", marker, track.name)));
    }

    let details = Paragraph::new(Text::from(lines))
        .style(normal_text_style())
        .block(
            Block::default()
                .title("Summary")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(details, chunks[0]);

    let items: Vec<ListItem> = app
        .done_menu_items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let prefix = if i == app.done_menu_index { ">" } else { " " };
            let style = if i == app.done_menu_index {
                Style::default()
                    .fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Gray)
            };
            ListItem::new(Line::from(Span::styled(
                format!("  {} {}", prefix, item),
                style,
            )))
        })
        .collect();

    let menu = List::new(items)
        .block(
            Block::default()
                .title("Next")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(Style::default());
    frame.render_widget(menu, chunks[1]);
}

fn render_download_block(
    frame: &mut ratatui::Frame,
    area: Rect,
    focused: bool,
    status: &str,
    download_dir: &Path,
) {
    let mut title = "Download".to_string();
    if focused {
        title.push_str(" (Enter)");
    }

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        });
    let mut lines = vec![
        Line::from(Span::styled(
            "[ Download ]",
            normal_text_style(),
        )),
        Line::from(format!("Save to: {}", download_dir.display())),
    ];
    if !status.trim().is_empty() {
        lines.push(Line::from(Span::styled(
            status,
            normal_text_style(),
        )));
    }
    let content = Paragraph::new(Text::from(lines))
        .style(normal_text_style())
        .alignment(Alignment::Center)
        .block(block);

    frame.render_widget(content, area);
}

fn login_flow() -> Result<()> {
    let user = kv_core::prompt::prompt("Username: ", false)?;
    let pass = kv_core::prompt::prompt("Password: ", true)?;

    keystore::Keystore::login(&user, &pass)?;

    let config = driver::Config {
        domain: "www.karaoke-version.com".to_string(),
        headless: false,
        download_path: None,
        idle_browser_timeout: Duration::from_secs(300),
        profile_dir: driver::default_profile_dir(),
    };
    tracing::info!("Launching Chromium for login (headless: false)");
    let driver = driver::Driver::new(config);
    driver.sign_in(&user, &pass)?;

    Ok(())
}

/// Remove abandoned temporary staging folders using the existing lock-aware
/// cleanup. Active and unmarked legacy folders are left untouched.
fn cleanup_abandoned_staging(download_root: &Path) -> Result<()> {
    let summary = tasks::download_song::cleanup_abandoned_kvui_staging_folders(download_root)?;
    tracing::info!(
        "Staging cleanup: {} removed, {} active, {} legacy, {} unsafe, {} errors",
        summary.removed,
        summary.skipped_active,
        summary.skipped_legacy,
        summary.skipped_unsafe,
        summary.errors
    );
    Ok(())
}

/// Phase 2C: download every selected HTTP stem without consulting either
/// historical progress tracker or C23 Resume. Each accepted stem is checked
/// against the requested name and key before independent safe publication.
fn run_stateless_http_download(
    song_url: &str,
    count_in: bool,
    transpose: i8,
    selected_tracks: Vec<String>,
    download_root: &Path,
    song_destination: &Path,
    policy: tasks::download_song::ExistingFilePolicy,
    published: std::sync::mpsc::Sender<String>,
) -> Result<()> {
    let domain = extract_domain_from_url(song_url)
        .ok_or_else(|| anyhow!("Missing domain from URL"))?;

    cleanup_abandoned_staging(download_root)?;
    let staging = tasks::download_song::create_download_staging_dir(download_root)?;
    let lock = tasks::download_song::lock_new_kvui_staging_directory(&staging)?;

    let options = tasks::download_song::DownloadOptions {
        count_in,
        transpose,
        selected_tracks: Some(selected_tracks),
        force_restart: false,
    };

    let result = tasks::download_song::download_song_http_stateless_with_cached_copy_hook(
        song_url,
        options,
        Some(staging.to_string_lossy().into_owned()),
        &domain,
        song_destination,
        &mut |track, staged_file| {
            let outcome = tasks::download_song::publish_single_staged_track(
                staged_file,
                song_destination,
                track,
                transpose,
                policy,
            )?;
            let filename = staged_file.file_name()
                .ok_or_else(|| anyhow!("Accepted stem has no filename"))?;
            let retained_path = song_destination.join(filename);
            if !retained_path.is_file() {
                return Err(anyhow!("Published stem is not available: {}", retained_path.display()));
            }
            tracing::info!("Published '{}' ({:?})", track, outcome);
            let _ = published.send(track.to_string());
            Ok(retained_path)
        },
    );

    // The original, safely published files are never removed by staging cleanup.
    drop(lock);
    if let Err(err) = fs::remove_dir_all(&staging) {
        tracing::warn!("Could not remove temporary staging {}: {}", staging.display(), err);
        if result.is_ok() {
            return Err(err.into());
        }
    }
    result
}

fn extract_domain_from_url(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(|h| h.to_string()))
}

fn is_authenticated() -> bool {
    match cookie_value() {
        Ok(cookie) => cookie.contains("|u-i:"),
        Err(_) => false,
    }
}

fn cookie_value() -> Result<String> {
    if let Ok(value) = std::env::var("KV_SESSION_COOKIE") {
        if value.trim().is_empty() {
            return Err(anyhow!("KV_SESSION_COOKIE is empty"));
        }
        return Ok(value);
    }

    keystore::Keystore::get_auth_cookie_value()
}

fn sync_song_catalog(cookie: &str, full_resync: bool) -> Result<Vec<Song>> {
    let catalog = catalog::SongCatalog::open()?;
    let mut first_url = Url::parse("https://www.karaoke-version.com/my/download.html")?;
    first_url
        .query_pairs_mut()
        .append_pair("orderField", "add_date")
        .append_pair("orderSort", "desc");

    let mut next_url = Some(first_url.to_string());
    let mut discovered = Vec::new();
    let mut visited_pages = std::collections::HashSet::new();
    while let Some(page_url) = next_url.take() {
        if !visited_pages.insert(page_url.clone()) {
            break;
        }
        tracing::info!("Syncing songs page {}", page_url);
        let (songs, next) = fetch_songs_page(cookie, &page_url)?;
        let mut reached_cached_song = false;
        for song in songs {
            if !full_resync && catalog.contains(&song.url)? {
                reached_cached_song = true;
                break;
            }
            discovered.push(song);
        }
        if reached_cached_song {
            tracing::info!("Reached previously cached song; incremental sync complete");
            break;
        }
        next_url = next;
        if visited_pages.len() >= 500 {
            return Err(anyhow!("Stopped song sync after 500 pages"));
        }
    }

    if full_resync {
        catalog.clear()?;
    }
    catalog.upsert(&discovered)?;
    let songs = catalog.list()?;
    tracing::info!(
        "Song catalog sync complete: {} discovered, {} cached total",
        discovered.len(),
        songs.len()
    );
    Ok(songs)
}

fn fetch_songs_page(cookie: &str, page_url: &str) -> Result<(Vec<Song>, Option<String>)> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("kv-downloader-tui")
        .build()?;

    let res = client
        .get(page_url)
        .header("Cookie", format!("karaoke-version={}", cookie))
        .send()?;

    let final_url = res.url().clone();
    let mut body = res.text()?;
    if is_browser_verification_page(&body) {
        tracing::warn!("Browser verification interrupted song browsing; opening Chromium");
        body = fetch_page_in_visible_browser(
            page_url,
            cookie,
            "my-downloaded-files",
        )?;
    }
    if is_login_page(&body, final_url.as_str()) {
        return Err(anyhow!(
            "Your saved session is no longer accepted. Choose Login to refresh it."
        ));
    }
    let doc = Html::parse_document(&body);

    let row_selector = Selector::parse("table.my-downloaded-files tr.vam").unwrap();
    let song_selector = Selector::parse("td.my-downloaded-files__song a").unwrap();
    let artist_selector = Selector::parse("td.my-downloaded-files__artist a").unwrap();
    let date_selector = Selector::parse("td.my-downloaded-files__date").unwrap();

    let mut songs = Vec::new();
    for row in doc.select(&row_selector) {
        let song_el = row.select(&song_selector).next();
        let artist_el = row.select(&artist_selector).next();

        if let (Some(song_el), Some(artist_el)) = (song_el, artist_el) {
            let title = normalize_text(song_el.text());
            let artist = normalize_text(artist_el.text());
            let href = song_el.value().attr("href").unwrap_or("");
            if title.is_empty() || href.is_empty() {
                continue;
            }
            let url = Url::parse("https://www.karaoke-version.com")
                .and_then(|base| base.join(href))
                .map(|u| u.to_string())
                .unwrap_or_else(|_| href.to_string());

            let purchase_date = row
                .select(&date_selector)
                .next()
                .map(|date| normalize_text(date.text()))
                .and_then(|date| parse_purchase_date(&date))
                .unwrap_or(0);
            songs.push(Song {
                title,
                artist,
                url,
                first_seen: 0,
                purchase_date,
            });
        }
    }

    if songs.is_empty() {
        write_kvui_debug_page("kvui_songs.html", &body);
        return Err(anyhow!(
            "The download page loaded, but its song list was not recognized. Set KV_HTTP_DEBUG=1 and inspect kvui_songs.html."
        ));
    }

    let next_url = find_next_songs_page(&doc, page_url);
    Ok((songs, next_url))
}

fn find_next_songs_page(doc: &Html, page_url: &str) -> Option<String> {
    let selector = Selector::parse("a[rel='next'], a.next, .pagination a, .pager a").ok()?;
    let base = Url::parse(page_url).ok()?;
    for link in doc.select(&selector) {
        let text = normalize_text(link.text()).to_lowercase();
        let class = link.value().attr("class").unwrap_or("").to_lowercase();
        let rel = link.value().attr("rel").unwrap_or("").to_lowercase();
        let is_next = rel.contains("next")
            || class.contains("next")
            || text.contains("next")
            || text.contains("read all")
            || text == ">"
            || text == "›";
        if is_next {
            if let Some(href) = link.value().attr("href") {
                if let Ok(mut url) = base.join(href) {
                    if !url.query_pairs().any(|(key, _)| key == "orderField") {
                        url.query_pairs_mut()
                            .append_pair("orderField", "add_date")
                            .append_pair("orderSort", "desc");
                    }
                    return Some(url.to_string());
                }
            }
        }
    }
    None
}

fn parse_purchase_date(value: &str) -> Option<i64> {
    let mut parts = value.split('/');
    let month = parts.next()?.parse::<i64>().ok()?;
    let day = parts.next()?.parse::<i64>().ok()?;
    let year = parts.next()?.parse::<i64>().ok()?;
    let year = if year < 100 { 2000 + year } else { year };
    Some(year * 10_000 + month * 100 + day)
}

fn is_browser_verification_page(html: &str) -> bool {
    kv_core::challenge::is_challenge_html(html)
}

fn fetch_page_in_visible_browser(
    url: &str,
    session_cookie: &str,
    expected_content: &str,
) -> Result<String> {
    let config = driver::Config {
        domain: "www.karaoke-version.com".to_string(),
        headless: false,
        download_path: None,
        idle_browser_timeout: Duration::from_secs(300),
        profile_dir: driver::default_profile_dir(),
    };
    let driver = driver::Driver::new(config);
    let tab = driver.browser.new_tab()?;
    tab.navigate_to("https://www.karaoke-version.com")?
        .wait_until_navigated()?;
    driver.set_session_cookie(&tab, session_cookie)?;
    tab.navigate_to(url)?.wait_until_navigated()?;

    tracing::warn!(
        "Complete browser verification or sign in in Chromium. Waiting for the requested page..."
    );
    let started = Instant::now();
    let mut last_navigation = Instant::now();
    loop {
        let html = tab.get_content()?;
        if html.contains(expected_content) {
            tracing::info!("Requested page loaded in Chromium");
            return Ok(html);
        }

        if started.elapsed() >= Duration::from_secs(300) {
            return Err(anyhow!("Timed out waiting for the requested page in Chromium"));
        }

        if !is_browser_verification_page(&html)
            && !is_login_page(&html, &tab.get_url())
            && last_navigation.elapsed() >= Duration::from_secs(3)
        {
            tab.navigate_to(url)?.wait_until_navigated()?;
            last_navigation = Instant::now();
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn is_login_page(html: &str, final_url: &str) -> bool {
    final_url.contains("/my/login")
        || html.contains("id=\"frm_login\"")
        || html.contains("name=\"frm_login\"")
}

fn write_kvui_debug_page(filename: &str, html: &str) {
    if std::env::var("KV_HTTP_DEBUG").ok().as_deref() != Some("1") {
        return;
    }
    if let Err(err) = std::fs::write(filename, html) {
        tracing::warn!("Failed to write {}: {}", filename, err);
    } else {
        tracing::debug!("Wrote debug page to {}", filename);
    }
}

fn fetch_song_details(cookie: &str, song_url: &str) -> Result<SongDetails> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("kv-downloader-tui")
        .build()?;

    let res = client
        .get(song_url)
        .header("Cookie", format!("karaoke-version={}", cookie))
        .send()?;

    let final_url = res.url().clone();
    let mut body = res.text()?;
    if is_browser_verification_page(&body) {
        tracing::warn!("Browser verification interrupted track loading; opening Chromium");
        body = fetch_page_in_visible_browser(song_url, cookie, "track__caption")?;
    }
    if is_login_page(&body, final_url.as_str()) {
        return Err(anyhow!(
            "Your saved session is no longer accepted. Choose Login to refresh it."
        ));
    }
    write_kvui_debug_page("kvui_song.html", &body);
    let doc = Html::parse_document(&body);

    let track_selector = Selector::parse("div.track").unwrap();
    let caption_selector = Selector::parse(".track__caption").unwrap();
    let precount_selector = Selector::parse("#precount").unwrap();
    let audio_info_selector = Selector::parse("#audio-infos p").unwrap();

    let mut tracks = Vec::new();
    for track in doc.select(&track_selector) {
        let caption = match track.select(&caption_selector).next() {
            Some(caption) => caption,
            None => continue,
        };

        let segments: Vec<&str> = caption.text().collect();
        let has_precount = caption.select(&precount_selector).next().is_some();
        let name = if has_precount {
            segments
                .iter()
                .rev()
                .find(|s| !s.trim().is_empty())
                .map(|s| normalize_text(std::iter::once(*s)))
                .unwrap_or_default()
        } else {
            normalize_text(segments.iter().copied())
        };
        if name.is_empty() || is_precount_track_name(&name) {
            continue;
        }

        tracks.push(name);
    }

    if tracks.is_empty() {
        write_kvui_debug_page("kvui_song.html", &body);
        return Err(anyhow!(
            "The song page loaded, but its tracks were not recognized. Set KV_HTTP_DEBUG=1 and inspect kvui_song.html."
        ));
    }

    let base_key = extract_base_key(&doc, &audio_info_selector);

    Ok(SongDetails { tracks, base_key })
}

fn normalize_text<'a, I>(segments: I) -> String
where
    I: Iterator<Item = &'a str>,
{
    let joined = segments.collect::<Vec<_>>().join(" ");
    joined
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn update_download_state(app: &mut App) {
    let Some(download) = app.download.as_mut() else {
        return;
    };
    if download.done.is_some() {
        return; // Final results must not be overwritten by later log parsing.
    }

    // Completion is determined by the protected download worker,
    // not by saved progress from earlier download requests.

    let log_progress = track_log_progress(app.logs.clone());
    for track in &mut download.tracks {
        if track.status == TrackStatus::Done {
            continue;
        }
        let normalized = normalize_track_name(&track.name);
        if log_progress.failed.contains(&normalized) {
            track.status = TrackStatus::Failed;
            track.attempt = 3;
        } else if log_progress.current.as_deref() == Some(normalized.as_str()) {
            track.status = TrackStatus::Downloading;
            track.attempt = log_progress.attempts.get(&normalized).copied().unwrap_or(1);
        } else {
            track.status = TrackStatus::Pending;
            track.attempt = log_progress.attempts.get(&normalized).copied().unwrap_or(0);
        }
    }

    // Log parsing is informational; only actual publication events certify
    // a completed HTTP stem. Apply these *after* parsing so they stay Done.
    if let Some(receiver) = download.published_rx.as_ref() {
        for published_name in receiver.try_iter() {
            let requested_name = normalize_track_name(&published_name);
            if let Some(track) = download.tracks.iter_mut().find(|track| {
                normalize_track_name(&track.name).eq_ignore_ascii_case(&requested_name)
            }) {
                track.status = TrackStatus::Done;
            }
        }
    }

    if let Some(handle) = download.handle.as_ref() {
        if handle.is_finished() {
            let result = download.handle.take().unwrap().join();
            match result {
                Ok(Ok(())) => {
                    if download.published_rx.is_some() {
                        // Every selected HTTP stem must have a publication event.
                        let missing = download.tracks.iter()
                            .filter(|track| track.status != TrackStatus::Done)
                            .map(|track| track.name.clone())
                            .collect::<Vec<_>>();
                        if !missing.is_empty() {
                            download.done = Some(Err(format!(
                                "HTTP worker finished without publishing: {}", missing.join(", ")
                            )));
                            app.status = "Download finished with unverified tracks".to_string();
                        } else {
                            download.done = Some(Ok(()));
                            app.status = "Download complete".to_string();
                        }
                    } else {
                        // Browser path retains its existing completion handling.
                        for track in &mut download.tracks {
                            if track.status == TrackStatus::Pending
                                || track.status == TrackStatus::Downloading
                            {
                                track.status = TrackStatus::Done;
                            }
                        }
                        download.done = Some(Ok(()));
                        app.status = "Download complete".to_string();
                    }
                    download.duration = Some(download.started_at.elapsed());
                    app.done_menu_index = 0;
                    app.screen = Screen::DownloadDone;
                }
                Ok(Err(err)) => {
                    for track in &mut download.tracks {
                        if track.status == TrackStatus::Downloading {
                            track.status = TrackStatus::Failed;
                        }
                    }
                    tracing::error!("Download failed: {}", err);
                    download.done = Some(Err(err.to_string()));
                    download.duration = Some(download.started_at.elapsed());
                    app.status = format!("Download finished with errors: {}", err);
                    app.done_menu_index = 0;
                    app.screen = Screen::DownloadDone;
                }
                Err(_) => {
                    tracing::error!("Download thread panicked");
                    download.done = Some(Err("Download thread panicked".to_string()));
                    download.duration = Some(download.started_at.elapsed());
                    app.status = "Download failed: download thread panicked".to_string();
                    app.done_menu_index = 0;
                    app.screen = Screen::DownloadDone;
                }
            }
        }
    }
}

fn handle_download_keys(app: &mut App, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Up => {
            app.log_follow = false;
            app.log_scroll = app.log_scroll.saturating_sub(1);
        }
        KeyCode::Down => {
            app.log_follow = false;
            app.log_scroll = app.log_scroll.saturating_add(1);
        }
        KeyCode::End => {
            app.log_follow = true;
        }
        KeyCode::Esc => {
            if app.download.as_ref().and_then(|d| d.done.as_ref()).is_some() {
                app.screen = Screen::Menu;
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_done_keys(app: &mut App, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Up => {
            if app.done_menu_index > 0 {
                app.done_menu_index -= 1;
            }
        }
        KeyCode::Down => {
            if app.done_menu_index + 1 < app.done_menu_items.len() {
                app.done_menu_index += 1;
            }
        }
        KeyCode::Enter => {
            let selected = app
                .done_menu_items
                .get(app.done_menu_index)
                .cloned()
                .unwrap_or_default();
            match selected.as_str() {
                "Open in File Explorer" => {
                    if let Some(download) = &app.download {
                        let _ = open_in_file_explorer(&download.download_dir);
                    }
                }
                "Back to Songs" => {
                    app.screen = Screen::Songs;
                }
                "Quit" => return Ok(true),
                _ => {}
            }
        }
        KeyCode::Esc => {
            app.screen = Screen::Songs;
        }
        _ => {}
    }
    Ok(false)
}

struct TrackLogProgress {
    current: Option<String>,
    attempts: std::collections::HashMap<String, u8>,
    failed: std::collections::HashSet<String>,
}

fn track_log_progress(logs: Arc<Mutex<LogBuffer>>) -> TrackLogProgress {
    let mut progress = TrackLogProgress {
        current: None,
        attempts: std::collections::HashMap::new(),
        failed: std::collections::HashSet::new(),
    };
    let Ok(buf) = logs.lock() else {
        return progress;
    };

    for line in &buf.lines {
        if line.contains("Processing track") {
            if let Some(track) = quoted_track_name(line) {
                progress.current = Some(normalize_track_name(&track));
            }
        }
        if let Some(index) = line.find("Attempt ") {
            let attempt = line[index + "Attempt ".len()..]
                .split('/')
                .next()
                .and_then(|value| value.parse::<u8>().ok());
            if let (Some(attempt), Some(track)) = (attempt, quoted_track_name(line)) {
                let track = normalize_track_name(&track);
                progress.attempts.insert(track.clone(), attempt);
                progress.current = Some(track);
            }
        }
        if line.contains("Giving up on") {
            if let Some(track) = quoted_track_name(line) {
                progress.failed.insert(normalize_track_name(&track));
            }
        }
    }
    progress
}

fn quoted_track_name(line: &str) -> Option<String> {
    let start = line.find('\'')?;
    let rest = &line[start + 1..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

fn is_precount_track_name(value: &str) -> bool {
    let normalized = value.to_lowercase();
    normalized.contains("intro count") || normalized.contains("precount")
}

fn normalize_track_name(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TrackStatus {
    Pending,
    Downloading,
    Done,
    Failed,
}

#[derive(Clone, Debug)]
struct TrackStatusItem {
    name: String,
    status: TrackStatus,
    attempt: u8,
}

struct DownloadState {
    song: Song,
    count_in: bool,
    transpose: i8,
    tracks: Vec<TrackStatusItem>,
    handle: Option<JoinHandle<Result<()>>>,
    // Per-run publications reported by the HTTP worker, independent of Resume.
    published_rx: Option<std::sync::mpsc::Receiver<String>>,
    done: Option<Result<(), String>>,
    started_at: Instant,
    duration: Option<Duration>,
    download_dir: std::path::PathBuf,
}

struct LogBuffer {
    lines: Vec<String>,
    partial: String,
    max: usize,
}

enum LoadingKind {
    Songs,
    Tracks { song: Song },
}

struct LoadingState {
    message: String,
    kind: LoadingKind,
    receiver: std::sync::mpsc::Receiver<LoadingResult>,
}

enum LoadingResult {
    Songs(Result<Vec<Song>, String>),
    Tracks(Result<SongDetails, String>),
}

struct ConfirmState {
    message: Vec<String>,
    selected: usize,
    payload: ConfirmPayload,
}

enum ConfirmPayload {
    StartDownload {
        song: Song,
        selected_tracks: Vec<String>,
        intro_click: bool,
        key_shift: i8,
    },
}

impl LogBuffer {
    fn new(max: usize) -> Self {
        Self {
            lines: Vec::new(),
            partial: String::new(),
            max,
        }
    }

    fn push_line(&mut self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        self.lines.push(line);
        if self.lines.len() > self.max {
            let excess = self.lines.len() - self.max;
            self.lines.drain(0..excess);
        }
    }
}

struct LogWriter {
    buffer: Arc<Mutex<LogBuffer>>,
    diagnostics: Option<Arc<Mutex<File>>>,
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let mut guard = self
            .buffer
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "log buffer poisoned"))?;
        guard.partial.push_str(&text);
        let mut completed_lines = Vec::new();
        while let Some(pos) = guard.partial.find('\n') {
            let line = guard.partial.drain(..=pos).collect::<String>();
            let line = line.trim_end().to_string();
            guard.push_line(line.clone());
            completed_lines.push(line);
        }
        drop(guard);

        if let Some(diagnostics) = &self.diagnostics {
            let mut file = diagnostics
                .lock()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "diagnostics file poisoned"))?;
            for line in completed_lines {
                writeln!(file, "{}", strip_ansi_codes(&line))?;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics
                .lock()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "diagnostics file poisoned"))?
                .flush()?;
        }
        Ok(())
    }
}

struct LogWriterFactory {
    buffer: Arc<Mutex<LogBuffer>>,
    diagnostics: Option<Arc<Mutex<File>>>,
}

impl<'a> MakeWriter<'a> for LogWriterFactory {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter {
            buffer: self.buffer.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }
}

struct LogScope {
    _buffer: Arc<Mutex<LogBuffer>>,
}

impl LogScope {
    fn new(buffer: Arc<Mutex<LogBuffer>>) -> Self {
        if let Ok(mut guard) = buffer.lock() {
            guard.push_line("---- download started ----".to_string());
        }
        Self { _buffer: buffer }
    }
}

fn setup_tracing(buffer: Arc<Mutex<LogBuffer>>, diagnostics: Option<Arc<Mutex<File>>>) {
    let detailed = diagnostics.is_some();
    let writer = LogWriterFactory {
        buffer,
        diagnostics,
    };
    let filter = if detailed {
        EnvFilter::new("info,kvui=debug,kv_core=debug")
    } else {
        EnvFilter::new("info")
    };
    let _ = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(true)
        .with_env_filter(filter)
        .try_init();
}

fn strip_ansi_codes(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

fn parse_ansi_line(input: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current = Style::default();
    let mut buf = String::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), current));
            }
            let mut code = String::new();
            while let Some(c) = chars.next() {
                if c == 'm' {
                    break;
                }
                code.push(c);
            }
            apply_sgr(&code, &mut current);
        } else {
            buf.push(ch);
        }
    }

    if !buf.is_empty() {
        spans.push(Span::styled(buf, current));
    }

    Line::from(spans)
}

fn apply_sgr(code: &str, style: &mut Style) {
    if code.is_empty() {
        *style = Style::default();
        return;
    }

    for part in code.split(';') {
        if let Ok(num) = part.parse::<u16>() {
            match num {
                0 => *style = Style::default(),
                1 => *style = style.add_modifier(Modifier::BOLD),
                2 => *style = style.add_modifier(Modifier::DIM),
                22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
                30..=37 => *style = style.fg(Color::Indexed((num - 30) as u8)),
                39 => *style = style.fg(Color::Reset),
                40..=47 => *style = style.bg(Color::Indexed((num - 40) as u8)),
                49 => *style = style.bg(Color::Reset),
                90..=97 => *style = style.fg(Color::Indexed((num - 90 + 8) as u8)),
                100..=107 => *style = style.bg(Color::Indexed((num - 100 + 8) as u8)),
                _ => {}
            }
        }
    }
}

fn start_loading_songs(app: &mut App, cookie: String, full_resync: bool) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = sync_song_catalog(&cookie, full_resync).map_err(|e| e.to_string());
        let _ = tx.send(LoadingResult::Songs(result));
    });
    app.loading = Some(LoadingState {
        message: if full_resync {
            "Fully resyncing your song catalog...".to_string()
        } else {
            "Syncing your song catalog...".to_string()
        },
        kind: LoadingKind::Songs,
        receiver: rx,
    });
}

fn start_loading_tracks(app: &mut App, cookie: String, song: Song) {
    let song_url = song.url.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = fetch_song_details(&cookie, &song_url).map_err(|e| e.to_string());
        let _ = tx.send(LoadingResult::Tracks(result));
    });
    app.loading = Some(LoadingState {
        message: format!("Loading tracks for {}...", song.title),
        kind: LoadingKind::Tracks { song },
        receiver: rx,
    });
}

fn update_loading_state(app: &mut App) {
    let Some(loading) = app.loading.as_mut() else {
        return;
    };

    match loading.receiver.try_recv() {
        Ok(result) => {
            let kind = match &loading.kind {
                LoadingKind::Songs => LoadingKind::Songs,
                LoadingKind::Tracks { song } => LoadingKind::Tracks { song: song.clone() },
            };
            app.loading = None;
            match (kind, result) {
                (LoadingKind::Songs, LoadingResult::Songs(Ok(songs))) => {
                    let count = songs.len();
                    set_song_list(app, songs);
                    app.status = format!("{} songs cached locally", count);
                }
                (LoadingKind::Songs, LoadingResult::Songs(Err(err))) => {
                    app.status = format!("Failed to load songs: {}", err);
                }
                (LoadingKind::Tracks { song }, LoadingResult::Tracks(Ok(details))) => {
                    app.tracks = details
                        .tracks
                        .into_iter()
                        .map(|name| Track {
                            name,
                            selected: true,
                        })
                        .collect();
                    app.tracks_state = ListState::default();
                    if !app.tracks.is_empty() {
                        app.tracks_state.select(Some(0));
                    }
                    app.current_song = Some(song.clone());
                    app.intro_click = false;
                    app.key_shift = 0;
                    app.base_key = details.base_key;
                    app.detail_focus = DetailFocus::Tracks;
                    app.screen = Screen::SongDetail;
                    // Saved Resume selections are intentionally not offered.
                }
                (LoadingKind::Tracks { .. }, LoadingResult::Tracks(Err(err))) => {
                    app.status = format!("Failed to load tracks: {}", err);
                }
                _ => {
                    app.status = "Unexpected loading result".to_string();
                }
            }
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {}
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            app.loading = None;
            app.status = "Loading failed (channel closed)".to_string();
        }
    }
}

fn render_loading_overlay(frame: &mut ratatui::Frame, app: &App) {
    let Some(loading) = &app.loading else {
        return;
    };
    let area = centered_rect(60, 20, frame.size());
    let spinner = ["|", "/", "-", "\\"][app.spinner_index % 4];
    let text = vec![
        Line::from(Span::styled(
            "Please wait",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(Span::raw("")),
        Line::from(Span::styled(
            format!("{} {}", spinner, loading.message),
            normal_text_style(),
        )),
    ];

    frame.render_widget(Clear, area);
    let block = Block::default()
        .title("Working")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let panel = Paragraph::new(Text::from(text))
        .block(block)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    frame.render_widget(panel, area);
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
    let vertical = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1]);
    vertical[1]
}

fn render_recovery_overlay(frame: &mut ratatui::Frame, app: &App) {
    if app.recovery_cleanup_confirm_open {
        render_recovery_cleanup_confirmation(frame, app);
        return;
    }
    if app.recovery_details_open {
        render_recovery_details_overlay(frame, app);
        return;
    }
    if !app.recovery_notice_open {
        return;
    }

    let area = centered_rect(85, 60, frame.size());
    frame.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(""),
        Line::from(
            "KVUI found temporary files left by an interrupted download."
        ).alignment(Alignment::Center),
        Line::from(""),
        Line::from(format!(
            "  Incomplete folders found: {}",
            app.incomplete_folders.len()
        )),
        Line::from(""),
        Line::from(
            "  Cleaning these files will not affect any completed downloads."
        ),
        Line::from(""),
        Line::from(Span::styled(
            "  Options:",
            Style::default().remove_modifier(Modifier::BOLD),
        )),
    ];

    let options = [
        ("Clean Up", "Removes the abandoned temp folder and files"),
        ("Remind Me Later", "Continue using KVUI"),
        ("View Details", "See the files, sizes, and location"),
    ];

    for (index, (label, description)) in options.iter().enumerate() {
        let text = format!("    {} - {}", label, description);

        let style = if app.recovery_selected == index {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .remove_modifier(Modifier::BOLD)
        } else {
            normal_text_style().remove_modifier(Modifier::BOLD)
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    let panel = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title("Incomplete Download Found")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });

    frame.render_widget(panel, area);
}
fn render_recovery_cleanup_confirmation(
    frame: &mut ratatui::Frame,
    app: &App,
) {
    let area = centered_rect(80, 50, frame.size());
    frame.render_widget(Clear, area);

    let mut lines = vec![
        Line::from("Remove abandoned temporary download files?"),
        Line::from(""),
        Line::from(format!(
            "Incomplete folders detected: {}",
            app.incomplete_folders.len()
        )),
        Line::from(""),
        Line::from("Only verified, unlocked staging folders are eligible."),
        Line::from("Active downloads and legacy folders will be preserved."),
        Line::from("Completed downloads will not be removed."),
        Line::from(""),
        Line::from("Yes permanently removes eligible temporary files."),
        Line::from(""),
    ];

    let choices = [
        "Yes - Clean Up",
        "No - Keep Files",
    ];

    for (index, choice) in choices.iter().enumerate() {
        let style = if app.recovery_cleanup_selected == index {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
        } else {
            normal_text_style()
        };

        lines.push(Line::from(Span::styled(
            format!("  {choice}  "),
            style,
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("Esc: Cancel"));

    let panel = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title("Confirm Temporary File Cleanup")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });

    frame.render_widget(panel, area);
}
fn render_recovery_details_overlay(frame: &mut ratatui::Frame, app: &App) {
    let area = centered_rect(92, 80, frame.size());
    frame.render_widget(Clear, area);

    let mut lines = vec![
        Line::from("Temporary files from interrupted downloads"),
        Line::from(""),
    ];

    for folder in &app.incomplete_folders {
        lines.push(Line::from(format!(
            "Folder: {}",
            folder.display()
        )));

        match tasks::download_song::inspect_kvui_staging_directory(folder) {
            Ok(items) => {
                if items.is_empty() {
                    lines.push(Line::from("  (empty folder)"));
                }

                for item in items {
                    let kind = if item.is_directory {
                        "[DIR]"
                    } else {
                        "[FILE]"
                    };

                    let size = item.size_bytes
                        .map(|bytes| format!("{bytes} bytes"))
                        .unwrap_or_else(|| "size unavailable".to_string());

                    lines.push(Line::from(format!(
                        "  {} {} - {}",
                        kind, item.name, size
                    )));
                }
            }
            Err(err) => {
                lines.push(Line::from(format!(
                    "  Could not inspect folder: {err}"
                )));
            }
        }

        lines.push(Line::from(""));
    }

    lines.push(Line::from(
        "Up/Down: Scroll | PgUp/PgDn: Scroll | Esc: Return"
    ));

    let scroll = app.recovery_detail_scroll
        .min(u16::MAX as usize) as u16;

    let panel = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title("Incomplete Download Details")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(panel, area);
}
fn download_confirmation_labels() -> &'static [&'static str] {
    &[" DOWNLOAD ", " CANCEL "]
}

fn download_confirmation_cancel_index() -> usize {
    download_confirmation_labels().len() - 1
}

#[cfg(test)]
mod c23_staging_startup_cleanup_tests {
    use super::{App, LogBuffer};
    use kv_core::tasks::download_song::lock_new_kvui_staging_directory;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn startup_scan_removes_abandoned_folder_before_warning() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kvui-c23-startup-cleanup-{}-{stamp}", std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let stale = root.join(".kvui-staging-123-456789-0");
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("partial.mp3"), b"unfinished").unwrap();
        let lock = lock_new_kvui_staging_directory(&stale).unwrap();
        drop(lock);

        let mut app = App::new(Arc::new(Mutex::new(LogBuffer::new(100))));
        app.download_dir = root.clone();
        app.scan_incomplete_downloads();
        app.refresh_menu();
        app.recovery_notice_open = !app.incomplete_folders.is_empty();

        assert!(!stale.exists());
        assert!(app.incomplete_folders.is_empty());
        assert!(!app.recovery_notice_open);
        assert!(!app.menu_items.iter().any(|item| item.starts_with("Incomplete Downloads (")));
        fs::remove_dir_all(root).unwrap();
    }
}

/// Render download choices as neutral text rows with one blank line between.
/// Only the currently selected choice receives a high-contrast highlight.
fn download_confirmation_choice_lines(
    labels: &[&str],
    selected: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        let is_selected = index == selected;
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
        } else {
            normal_text_style()
        };
        let pointer = if is_selected { ">" } else { " " };
        lines.push(Line::from(Span::styled(
            format!("  {pointer} {}  ", label.trim()),
            style,
        )));
    }
    lines
}

#[cfg(test)]
mod c24_download_confirm_layout_tests {
    use super::*;

    #[test]
    fn download_confirmation_has_only_download_and_cancel() {
        let labels = download_confirmation_labels();
        assert_eq!(labels, &[" DOWNLOAD ", " CANCEL "]);
        assert_eq!(download_confirmation_cancel_index(), 1);
        let lines = download_confirmation_choice_lines(labels, 0);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans[0].style.bg, Some(Color::Yellow));
        assert!(lines[1].spans.is_empty());
        assert_eq!(lines[2].spans[0].style.bg, None);
    }

    #[test]
    fn arrow_keys_navigate_download_choices_vertically() -> Result<()> {
        let mut app = App::new(Arc::new(Mutex::new(LogBuffer::new(20))));
        app.confirm = Some(ConfirmState {
            message: vec![],
            selected: 0,
            payload: ConfirmPayload::StartDownload {
                song: Song {
                    title: "Test".into(), artist: "Artist".into(),
                    url: "https://example.test/song".into(),
                    first_seen: 0, purchase_date: 0,
                },
                selected_tracks: vec!["Bass".into()],
                intro_click: false,
                key_shift: 0,
            },
        });
        handle_confirm_keys(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::empty()))?;
        assert_eq!(app.confirm.as_ref().unwrap().selected, 1);
        handle_confirm_keys(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::empty()))?;
        assert_eq!(app.confirm.as_ref().unwrap().selected, 0);
        handle_confirm_keys(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::empty()))?;
        assert_eq!(app.confirm.as_ref().unwrap().selected, 1); // Up from first choice wraps to Cancel.
        handle_confirm_keys(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::empty()))?;
        assert_eq!(app.confirm.as_ref().unwrap().selected, 0);
        Ok(())
    }
}

/// Download confirmation gets two lines below its heading, a blank line
/// after the method description, and two lines before the vertical options.
/// Other Yes/No dialogs preserve their existing spacing.
fn confirmation_intro_lines(message: &[String], download_confirm: bool) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "Please Confirm",
        Style::default().fg(Color::Yellow),
    ))];
    lines.push(Line::default());
    if download_confirm {
        lines.push(Line::default());
    }
    for (index, text) in message.iter().enumerate() {
        lines.push(Line::from(Span::raw(text.clone())));
        if download_confirm && index == 0 {
            lines.push(Line::default());
        }
    }
    lines.push(Line::default());
    if download_confirm {
        lines.push(Line::default());
    }
    lines
}

fn vertically_centered_confirmation(lines: Vec<Line<'static>>, height: usize) -> Vec<Line<'static>> {
    let top_padding = height.saturating_sub(lines.len()) / 2;
    let mut centered = Vec::with_capacity(lines.len() + top_padding);
    centered.extend(std::iter::repeat_with(Line::default).take(top_padding));
    centered.extend(lines);
    centered
}

#[cfg(test)]
mod c24_soft_text_theme_tests {
    use super::*;

    #[test]
    fn ordinary_text_is_gray_and_not_bold() {
        let style = normal_text_style();
        assert_eq!(style.fg, Some(Color::Gray));
        assert_eq!(style.bg, None);
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn highlighted_song_and_track_use_gold_text_without_white_background() {
        let style = selected_text_style();
        assert_eq!(style.fg, Some(Color::Yellow));
        assert_eq!(style.bg, None);
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn deselected_stems_are_dimmed_without_losing_gold_keyboard_focus() {
        let checked = track_name_style(false, true);
        let unchecked = track_name_style(false, false);
        let active_unchecked = track_name_style(true, false);
        assert_eq!(checked.fg, Some(Color::Gray));
        assert_eq!(unchecked.fg, Some(Color::DarkGray));
        assert_eq!(active_unchecked.fg, Some(Color::Yellow));
        for style in [checked, unchecked, active_unchecked] {
            assert_eq!(style.bg, None);
            assert!(!style.add_modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn unselected_download_choice_is_gray_but_selected_choice_keeps_gold_fill() {
        let rows = download_confirmation_choice_lines(&[" Download ", " Cancel "], 1);
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Gray));
        assert_eq!(rows[0].spans[0].style.bg, None);
        assert_eq!(rows[2].spans[0].style.bg, Some(Color::Yellow));
        assert!(!rows[2].spans[0].style.add_modifier.contains(Modifier::BOLD));
    }
}

#[cfg(test)]
mod c24_visual_readability_tests {
    use super::*;

    fn song(title: &str, artist: &str) -> Song {
        Song {
            title: title.into(), artist: artist.into(),
            url: format!("https://example.test/{title}"),
            first_seen: 1, purchase_date: 20260101,
        }
    }

    #[test]
    fn song_columns_align_and_swap_for_artist_sort() {
        let photograph = song("Photograph", "Def Leppard");
        let dirty = song("Dirty Laundry", "Don Henley");
        let title_rows = [
            song_list_row(&photograph, SongSort::Title, 25, 64),
            song_list_row(&dirty, SongSort::Title, 25, 64),
        ];
        assert_eq!(title_rows[0].find("Def Leppard"), Some(25));
        assert_eq!(title_rows[1].find("Don Henley"), Some(25));
        assert!(title_rows.iter().all(|row| row.contains('.')));

        let artist_row = song_list_row(&photograph, SongSort::Artist, 25, 64);
        assert!(artist_row.starts_with("Def Leppard"));
        assert_eq!(artist_row.find("Photograph"), Some(25));
        assert!(song_list_row(&photograph, SongSort::DateDescending, 25, 64)
            .starts_with("Photograph"));
    }

    #[test]
    fn blank_rows_are_not_songs_or_selectable() {
        let items = song_list_items(&[
            song("Photograph", "Def Leppard"),
            song("Dirty Laundry", "Don Henley"),
        ], SongSort::Title, 80);
        assert_eq!(items.len(), 4);
        // Each real song occupies the row immediately after an empty item.
        assert_eq!(song_display_index(0), 1);
        assert_eq!(song_display_index(1), 3);
    }

    #[test]
    fn narrow_song_columns_truncate_without_pushing_artist_out() {
        let row = song_list_row(
            &song("A Very Long Song Name", "A Very Long Artist Name"),
            SongSort::Title, 12, 30,
        );
        assert!(row.chars().count() <= 30);
        assert!(row.contains('…'));
    }

    #[test]
    fn download_confirmation_has_requested_blank_lines() {
        let lines = confirmation_intro_lines(&[
            "This will use the original HTTP downloader.".into(),
            "Choose Download Process".into(),
        ], true);
        assert_eq!(lines.len(), 8);
        assert_eq!(lines[0].spans[0].content.as_ref(), "Please Confirm");
        assert!(lines[1].spans.is_empty() && lines[2].spans.is_empty());
        assert!(lines[4].spans.is_empty());
        assert_eq!(lines[5].spans[0].content.as_ref(), "Choose Download Process");
        assert!(lines[6].spans.is_empty() && lines[7].spans.is_empty());
        let centered = vertically_centered_confirmation(lines, 14);
        assert_eq!(centered.len(), 11); // three lines of top padding.
        assert!(centered[..3].iter().all(|line| line.spans.is_empty()));
    }

    #[test]
    fn other_confirmation_dialog_spacing_remains_compact() {
        let lines = confirmation_intro_lines(&["Restore selection?".into()], false);
        assert_eq!(lines.len(), 4);
        assert!(lines[1].spans.is_empty());
        assert_eq!(lines[2].spans[0].content.as_ref(), "Restore selection?");
    }
}

fn render_confirm_overlay(frame: &mut ratatui::Frame, app: &App) {
    let Some(confirm) = &app.confirm else {
        return;
    };
    let download_confirm = true;
    // A vertical list needs more space than the old horizontal row.
    let height = if download_confirm { 80 } else { 35 };
    let area = centered_rect(70, height, frame.size());
    frame.render_widget(Clear, area);

    let mut lines = confirmation_intro_lines(&confirm.message, download_confirm);
    let labels: &[&str] = if download_confirm {
        download_confirmation_labels()
    } else {
        &[" YES ", " NO "]
    };

    if download_confirm {
        lines.extend(download_confirmation_choice_lines(labels, confirm.selected));
    } else {
        // Preserve the existing two-choice Yes/No dialogs without redesigning them.
        let mut choices = Vec::new();
        for (index, label) in labels.iter().enumerate() {
            let color = if index == 0 { Color::Green } else { Color::Red };
            let style = if confirm.selected == index {
                Style::default()
                    .fg(Color::Black)
                    .bg(color)
            } else {
                Style::default().fg(color)
            };
            choices.push(Span::styled((*label).to_string(), style));
            if index + 1 < labels.len() {
                choices.push(Span::raw(" "));
            }
        }
        lines.push(Line::from(choices));
    }

    // Keep the entire download-confirmation content centered vertically
    // within its already-centered overlay. Other dialogs retain their layout.
    if download_confirm {
        lines = vertically_centered_confirmation(lines, area.height.saturating_sub(2) as usize);
    }
    let panel = Paragraph::new(Text::from(lines))
        .style(normal_text_style())
        .block(
            Block::default()
                .title("Confirm")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    frame.render_widget(panel, area);
}

/// Handle incomplete-download recovery navigation.
/// File cleanup and details are not connected yet.
/// Perform cleanup only after explicit confirmation.
/// Individual failures do not stop checks of other staging folders.
fn run_confirmed_recovery_cleanup(app: &mut App) {
    use tasks::download_song::{
        cleanup_kvui_staging_directory,
        KvuiCleanupResult,
    };

    let folders = app.incomplete_folders.clone();
    let mut removed = 0usize;
    let mut active = 0usize;
    let mut legacy = 0usize;
    let mut unsafe_count = 0usize;
    let mut errors = 0usize;

    for folder in folders {
        match cleanup_kvui_staging_directory(&app.download_dir, &folder) {
            Ok(KvuiCleanupResult::Removed) => {
                removed += 1;
            }
            Ok(KvuiCleanupResult::SkippedActive) => {
                active += 1;
            }
            Ok(KvuiCleanupResult::SkippedLegacy) => {
                legacy += 1;
            }
            Ok(KvuiCleanupResult::SkippedUnsafe) => {
                unsafe_count += 1;
            }
            Err(err) => {
                errors += 1;
                tracing::warn!(
                    "Could not clean staging folder {}: {}",
                    folder.display(),
                    err
                );
            }
        }
    }

    app.scan_incomplete_downloads();
    app.refresh_menu();

    app.recovery_cleanup_confirm_open = false;
    app.recovery_notice_open = false;
    app.recovery_details_open = false;
    app.recovery_selected = 1;

    app.status = format!(
        "Cleanup: {} removed; {} legacy, {} active, {} unsafe preserved; {} errors. {} incomplete folders remain.",
        removed,
        legacy,
        active,
        unsafe_count,
        errors,
        app.incomplete_folders.len()
    );
}
fn handle_recovery_keys(app: &mut App, key: KeyEvent) -> Result<bool> {
    if app.recovery_details_open {
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => {
                app.recovery_details_open = false;
            }
            KeyCode::Up => {
                app.recovery_detail_scroll =
                    app.recovery_detail_scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                app.recovery_detail_scroll =
                    app.recovery_detail_scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                app.recovery_detail_scroll =
                    app.recovery_detail_scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                app.recovery_detail_scroll =
                    app.recovery_detail_scroll.saturating_add(10);
            }
            _ => {}
        }
        return Ok(false);
    }
    if app.recovery_cleanup_confirm_open {
        match key.code {
            KeyCode::Left | KeyCode::Up => {
                app.recovery_cleanup_selected = 0;
            }
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                app.recovery_cleanup_selected =
                    (app.recovery_cleanup_selected + 1) % 2;
            }
            KeyCode::Esc | KeyCode::Backspace => {
                app.recovery_cleanup_confirm_open = false;
            }
            KeyCode::Enter => {
                if app.recovery_cleanup_selected == 1 {
                    // No - Keep Files.
                    app.recovery_cleanup_confirm_open = false;
                } else {
                    run_confirmed_recovery_cleanup(app);
                }
            }
            _ => {}
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Up | KeyCode::Left => {
            app.recovery_selected =
                (app.recovery_selected + 2) % 3;
        }
        KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
            app.recovery_selected =
                (app.recovery_selected + 1) % 3;
        }
        KeyCode::Esc => {
            app.recovery_notice_open = false;
        }
        KeyCode::Enter => {
            if app.recovery_selected == 1 {
                // Remind Me Later.
                app.recovery_notice_open = false;
            }
            if app.recovery_selected == 2 {
                app.recovery_details_open = true;
                app.recovery_detail_scroll = 0;
            }
            if app.recovery_selected == 0 {
                app.recovery_cleanup_confirm_open = true;
                app.recovery_cleanup_selected = 1;
            }
            // Actual cleanup is not connected yet.
        }
        _ => {}
    }

    Ok(false)
}
fn handle_confirm_keys(app: &mut App, key: KeyEvent) -> Result<bool> {
    let Some(confirm) = &mut app.confirm else {
        return Ok(false);
    };
    match key.code {
        KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
            let count = download_confirmation_labels().len();

            confirm.selected = match key.code {
                KeyCode::Left | KeyCode::Up => (confirm.selected + count - 1) % count,
                _ => (confirm.selected + 1) % count,
            };
        }
        KeyCode::Esc => {
            app.confirm = None;
        }
        KeyCode::Enter => {
            let confirm = app.confirm.take().unwrap();

            if confirm.selected == download_confirmation_cancel_index() {
                return Ok(false);
            }

            // A selected stem is always verified, then safely replaced if present.
            let file_policy = tasks::download_song::ExistingFilePolicy::Replace;
            let ConfirmPayload::StartDownload {
                song,
                selected_tracks,
                intro_click,
                key_shift,
            } = confirm.payload;

            app.log_scroll = 0;
            app.log_follow = true;
            let download_dir = app.download_dir.clone();
            if !download_dir.is_dir() {
                app.status = format!(
                    "Download folder is unavailable: {}. Choose a new folder from the main menu.",
                    download_dir.display()
                );
                app.screen = Screen::SongDetail;
                return Ok(false);
            }
            let song_destination = match (|| -> Result<PathBuf> {
                kvui_check_destination_directory(&download_dir)?;
                kvui_select_keyed_song_destination(
                    &download_dir,
                    &song.artist,
                    &song.title,
                    &song.url,
                    key_shift,
                )
            })() {
                Ok(path) => path,
                Err(err) => {
                    app.status = format!("Cannot select song folder: {err}");
                    app.screen = Screen::SongDetail;
                    return Ok(false);
                }
            };
            // UI log history must not mislabel tracks in a later attempt.
            if let Ok(mut buffer) = app.logs.lock() {
                buffer.lines.clear();
                buffer.partial.clear();
            }
            let download_tracks = selected_tracks
                .iter()
                .map(|name| TrackStatusItem {
                    name: name.clone(),
                    status: TrackStatus::Pending,
                    attempt: 0,
                })
                .collect::<Vec<_>>();
            let song_url = song.url.clone();
            let song_artist = song.artist.clone();
            let song_title = song.title.clone();
            let logs = app.logs.clone();
            let (published_tx, published_rx) = std::sync::mpsc::channel::<String>();
            let worker_download_dir = download_dir.clone();
            let worker_song_destination = song_destination.clone();
            let handle = std::thread::spawn(move || {
                let _guard = LogScope::new(logs);
                kvui_prepare_keyed_song_destination(
                    &worker_download_dir,
                    &song_artist,
                    &song_title,
                    &song_url,
                    key_shift,
                    &worker_song_destination,
                )?;

                run_stateless_http_download(
                    &song_url,
                    intro_click,
                    key_shift,
                    selected_tracks,
                    &worker_download_dir,
                    &worker_song_destination,
                    file_policy,
                    published_tx,
                )
            });
            app.download = Some(DownloadState {
                song,
                count_in: intro_click,
                transpose: key_shift,
                tracks: download_tracks,
                handle: Some(handle),
                published_rx: Some(published_rx),
                done: None,
                started_at: Instant::now(),
                duration: None,
                download_dir: song_destination,
            });
            app.screen = Screen::DownloadStatus;
        }
        _ => {}
    }
    Ok(false)
}

fn extract_base_key(doc: &Html, selector: &Selector) -> Option<String> {
    for node in doc.select(selector) {
        let text = normalize_text(node.text());
        if let Some(rest) = text.strip_prefix("In the same key as the original:") {
            let key = rest.trim().split_whitespace().last().unwrap_or_default();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    None
}

fn format_key_display(base_key: Option<&str>, shift: i8) -> String {
    match base_key {
        Some(base) => {
            let transposed = transpose_key(base, shift);
            if shift == 0 {
                format!("0 ({})", transposed)
            } else {
                format!("{:+} ({})", shift, transposed)
            }
        }
        None => format!("{:+}", shift),
    }
}

fn transpose_key(base_key: &str, shift: i8) -> String {
    if shift == 0 {
        return base_key.to_string();
    }

    let base = base_key.trim();
    let (use_flats, normalized) = normalize_key(base);

    let sharp_scale = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let flat_scale = [
        "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
    ];

    let scale = if use_flats { &flat_scale } else { &sharp_scale };
    let idx = scale.iter().position(|k| *k == normalized);
    if let Some(idx) = idx {
        let shift = (shift as i32).rem_euclid(12) as usize;
        scale[(idx + shift) % scale.len()].to_string()
    } else {
        base_key.to_string()
    }
}

fn normalize_key(value: &str) -> (bool, &str) {
    let upper = value.trim();
    let use_flats = upper.contains('b');
    let normalized = match upper {
        "Cb" => "B",
        "B#" => "C",
        "Db" => "Db",
        "C#" => "C#",
        "Eb" => "Eb",
        "D#" => "D#",
        "Fb" => "E",
        "E#" => "F",
        "Gb" => "Gb",
        "F#" => "F#",
        "Ab" => "Ab",
        "G#" => "G#",
        "Bb" => "Bb",
        "A#" => "A#",
        "C" | "D" | "E" | "F" | "G" | "A" | "B" => upper,
        _ => upper,
    };
    (use_flats, normalized)
}

fn abbreviate_path(path: &Path, maximum_characters: usize) -> String {
    let value = path.display().to_string();
    let character_count = value.chars().count();
    if character_count <= maximum_characters {
        return value;
    }
    if maximum_characters <= 3 {
        return ".".repeat(maximum_characters);
    }

    let suffix = value
        .chars()
        .rev()
        .take(maximum_characters - 3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{suffix}")
}

fn prompt_download_folder(current: &Path) -> Result<Option<PathBuf>> {
    println!("Current download folder: {}", current.display());
    let value = kv_core::prompt::prompt(
        "New download folder (leave blank to keep the current folder): ",
        false,
    )?;
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }

    let path = expand_home_directory(value)?;
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let path = path
        .canonicalize()
        .map_err(|err| anyhow!("could not open {}: {err}", path.display()))?;
    if !path.is_dir() {
        return Err(anyhow!("{} is not a directory", path.display()));
    }
    if path.to_str().is_none() {
        return Err(anyhow!("the selected path is not valid UTF-8"));
    }

    Ok(Some(path))
}

fn expand_home_directory(value: &str) -> Result<PathBuf> {
    if value == "~" {
        return dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"));
    }
    if let Some(relative_path) = value.strip_prefix("~/") {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
        return Ok(home.join(relative_path));
    }
    Ok(PathBuf::from(value))
}

fn settings_path() -> Result<PathBuf> {
    let config_dir =
        dirs::config_dir().ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(config_dir.join("kv-downloader").join("kvui-settings"))
}

fn load_download_dir() -> Result<PathBuf> {
    load_download_dir_from(&settings_path()?)
}

fn load_download_dir_from(path: &Path) -> Result<PathBuf> {
    match fs::read_to_string(path) {
        Ok(value) => {
            let value = value.trim();
            if value.is_empty() {
                return Err(anyhow!("download folder setting is empty"));
            }
            Ok(PathBuf::from(value))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => default_download_dir()
            .ok_or_else(|| anyhow!("could not determine default download folder")),
        Err(err) => Err(err.into()),
    }
}

fn save_download_dir(download_dir: &Path) -> Result<()> {
    save_download_dir_to(&settings_path()?, download_dir)
}

fn save_download_dir_to(settings_path: &Path, download_dir: &Path) -> Result<()> {
    let parent = settings_path
        .parent()
        .ok_or_else(|| anyhow!("settings path has no parent directory"))?;
    fs::create_dir_all(parent)?;
    fs::write(settings_path, download_dir.to_string_lossy().as_bytes())?;
    Ok(())
}

fn default_download_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join("Downloads"))
}

fn open_in_file_explorer(path: &std::path::Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .status()
            .map_err(|e| anyhow!(e))?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .status()
            .map_err(|e| anyhow!(e))?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(path)
            .status()
            .map_err(|e| anyhow!(e))?;
    }
    Ok(())
}

fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{}m {}s", minutes, seconds)
}

#[cfg(test)]
mod song_catalog_tests {
    use super::{
        abbreviate_path, find_next_songs_page, load_download_dir_from, parse_purchase_date,
        save_download_dir_to, Html,
    };
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_purchase_date_for_local_sorting() {
        assert_eq!(parse_purchase_date("2/6/26"), Some(20260206));
        assert_eq!(parse_purchase_date("11/17/2025"), Some(20251117));
    }

    #[test]
    fn follows_next_page_and_preserves_date_sort() {
        let document = Html::parse_document(
            r#"<div class="pagination"><a rel="next" href="/my/download.html?page=2">Next &gt;</a></div>"#,
        );
        let next = find_next_songs_page(
            &document,
            "https://www.karaoke-version.com/my/download.html?orderField=add_date&orderSort=desc",
        )
        .unwrap();
        assert!(next.contains("page=2"));
        assert!(next.contains("orderField=add_date"));
        assert!(next.contains("orderSort=desc"));
    }

    #[test]
    fn abbreviates_long_download_folder_from_the_start() {
        let abbreviated = abbreviate_path(Path::new("/a/very/long/path/to/downloads"), 20);

        assert_eq!(abbreviated.chars().count(), 20);
        assert!(abbreviated.starts_with("..."));
        assert!(abbreviated.ends_with("to/downloads"));
        assert_eq!(abbreviate_path(Path::new("~/Downloads"), 20), "~/Downloads");
    }

    #[test]
    fn saves_and_loads_download_folder() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("kvui-settings-{}-{unique}", std::process::id()));
        let settings_path = directory.join("nested").join("kvui-settings");
        let download_dir = directory.join("downloads");

        save_download_dir_to(&settings_path, &download_dir).unwrap();

        assert_eq!(
            load_download_dir_from(&settings_path).unwrap(),
            download_dir
        );
        fs::remove_dir_all(directory).unwrap();
    }
}


/// Convert an artist or song title into a safe folder component.
/// Invalid Windows filename characters become underscores.
/// Empty or excessively long results are rejected.
#[allow(dead_code)]
fn kvui_safe_folder_component(raw: &str) -> Result<String> {
    let mut name = String::new();

    for ch in raw.trim().chars() {
        if ch.is_control()
            || matches!(
                ch,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            name.push('_');
        } else {
            name.push(ch);
        }
    }

    // Windows does not permit trailing periods or spaces.
    let mut name = name.trim_end_matches([' ', '.']).to_string();

    if name.is_empty() {
        return Err(anyhow!("Artist or song title produces an empty folder name"));
    }

    // Windows reserves these device names, even with extensions.
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();

    let reserved = matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
        | "COM1" | "COM2" | "COM3" | "COM4" | "COM5"
        | "COM6" | "COM7" | "COM8" | "COM9"
        | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5"
        | "LPT6" | "LPT7" | "LPT8" | "LPT9"
        | "CONIN$" | "CONOUT$"
        | "COM¹" | "COM²" | "COM³"
        | "LPT¹" | "LPT²" | "LPT³"
    );

    if reserved {
        name.push('_');
    }

    // Reject long titles rather than silently truncate them
    // and risk two different songs sharing the same folder.
    if name.chars().count() > 100 {
        return Err(anyhow!("Artist or song folder name is too long"));
    }

    Ok(name)
}

#[cfg(test)]
mod artist_song_folder_names {
    use super::kvui_safe_folder_component;

    #[test]
    fn preserves_normal_names() {
        assert_eq!(
            kvui_safe_folder_component("Eagles").unwrap(),
            "Eagles"
        );

        assert_eq!(
            kvui_safe_folder_component("Hotel California").unwrap(),
            "Hotel California"
        );
    }

    #[test]
    fn replaces_windows_invalid_characters() {
        assert_eq!(
            kvui_safe_folder_component("AC/DC").unwrap(),
            "AC_DC"
        );

        assert_eq!(
            kvui_safe_folder_component("Who: Me?").unwrap(),
            "Who_ Me_"
        );
    }

    #[test]
    fn removes_trailing_spaces_and_periods() {
        assert_eq!(
            kvui_safe_folder_component("  Queen  ").unwrap(),
            "Queen"
        );

        assert_eq!(
            kvui_safe_folder_component("My Song. ").unwrap(),
            "My Song"
        );
    }

    #[test]
    fn handles_reserved_windows_names() {
        assert_eq!(
            kvui_safe_folder_component("CON").unwrap(),
            "CON_"
        );

        assert_eq!(
            kvui_safe_folder_component("nul.txt").unwrap(),
            "nul.txt_"
        );
    }

    #[test]
    fn rejects_empty_and_unsafe_names() {
        assert!(kvui_safe_folder_component("").is_err());
        assert!(kvui_safe_folder_component("...").is_err());
        assert!(kvui_safe_folder_component("   ").is_err());

        let long_name = "A".repeat(101);
        assert!(kvui_safe_folder_component(&long_name).is_err());
    }
}


/// Construct an Artist/Song destination under the selected download root.
/// Does not create directories or modify files.
#[allow(dead_code)]
/// Build a song-folder name for the selected transposition.
/// Does not access or modify the filesystem.
#[allow(dead_code)]
fn kvui_keyed_song_folder_name(
    title: &str,
    transpose: i8,
) -> Result<String> {
    if !(-4..=4).contains(&transpose) {
        return Err(anyhow!("Unsupported key adjustment: {transpose}"));
    }

    let base = kvui_safe_folder_component(title)?;

    if transpose == 0 {
        return Ok(base);
    }

    let suffix = if transpose > 0 {
        format!("+{transpose}")
    } else {
        transpose.to_string()
    };

    // Preserve the existing 100-character folder-name limit.
    let max_base_length = 100 - suffix.chars().count();

    let prefix: String = base
        .chars()
        .take(max_base_length)
        .collect();

    let prefix = prefix.trim_end_matches([' ', '.']);

    if prefix.is_empty() {
        return Err(anyhow!("Song title is empty after shortening"));
    }

    Ok(format!("{prefix}{suffix}"))
}
fn kvui_song_destination_dir(
    download_root: &Path,
    artist: &str,
    title: &str,
) -> Result<PathBuf> {
    let artist_folder = kvui_safe_folder_component(artist)?;
    let song_folder = kvui_safe_folder_component(title)?;

    Ok(download_root.join(artist_folder).join(song_folder))
}

#[cfg(test)]
mod artist_song_destination_tests {
    use super::kvui_song_destination_dir;
    use std::path::Path;

    #[test]
    fn builds_artist_song_structure() {
        let root = Path::new("download-root");

        let result = kvui_song_destination_dir(
            root,
            "Eagles",
            "Hotel California",
        )
        .unwrap();

        assert_eq!(
            result,
            root.join("Eagles").join("Hotel California")
        );
    }

    #[test]
    fn sanitizes_folder_components() {
        let root = Path::new("download-root");

        let result = kvui_song_destination_dir(
            root,
            "AC/DC",
            "Who: Me?",
        )
        .unwrap();

        assert_eq!(
            result,
            root.join("AC_DC").join("Who_ Me_")
        );
    }

    #[test]
    fn rejects_invalid_components() {
        let root = Path::new("download-root");

        assert!(
            kvui_song_destination_dir(root, "", "Song").is_err()
        );

        assert!(
            kvui_song_destination_dir(root, "Artist", "...").is_err()
        );

        assert!(
            kvui_song_destination_dir(root, "Artist", "").is_err()
        );
    }
}


/// The source URL identifies the recording assigned to a song folder.
/// The marker will be written during the publishing integration step.
const KVUI_SONG_SOURCE_FILE: &str = ".kvui-song-source";

/// Check an existing directory without following unsafe folder links.
/// Returns false when the directory does not exist.
#[allow(dead_code)]
fn kvui_check_destination_directory(path: &Path) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(err) => return Err(err.into()),
    };

    if metadata.file_type().is_symlink() {
        return Err(anyhow!("Destination is a symbolic link: {}", path.display()));
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        // FILE_ATTRIBUTE_REPARSE_POINT includes Windows junctions.
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(anyhow!(
                "Destination is a Windows reparse point: {}",
                path.display()
            ));
        }
    }

    if !metadata.is_dir() {
        return Err(anyhow!(
            "Destination exists but is not a directory: {}",
            path.display()
        ));
    }

    Ok(true)
}

/// An existing folder belongs to this recording only when its
/// source marker matches the exact Karaoke-Version URL.
#[allow(dead_code)]
fn kvui_song_folder_matches_source(
    folder: &Path,
    song_url: &str,
) -> Result<bool> {
    let marker = folder.join(KVUI_SONG_SOURCE_FILE);

    match std::fs::read_to_string(marker) {
        Ok(value) => Ok(value == song_url.trim()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// Produce a repeatable alternative folder name when the normal
/// Artist/Song location already belongs to something else.
#[allow(dead_code)]
fn kvui_collision_song_folder(title: &str, song_url: &str) -> Result<String> {
    if song_url.trim().is_empty() {
        return Err(anyhow!("Song URL cannot be empty"));
    }

    // Stable FNV-1a fingerprint. Source markers provide the
    // authoritative collision check; the hash is only a suffix.
    let mut hash: u64 = 0xcbf29ce484222325;

    for byte in song_url.trim().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    let suffix = format!(" [KV-{hash:016x}]");
    let title = kvui_safe_folder_component(title)?;

    let limit = 100usize - suffix.chars().count();

    let prefix: String = title.chars().take(limit).collect();
    let prefix = prefix.trim_end_matches([' ', '.']);

    Ok(format!("{prefix}{suffix}"))
}

/// Select a destination without creating or modifying directories.
///
/// Existing unmarked folders are not considered KVUI-owned.
/// An alternate destination is selected in that situation.
/// An occupied alternate destination must have a matching marker.
#[allow(dead_code)]
fn kvui_select_song_destination(
    download_root: &Path,
    artist: &str,
    title: &str,
    song_url: &str,
) -> Result<PathBuf> {
    if song_url.trim().is_empty() {
        return Err(anyhow!("Song URL cannot be empty"));
    }

    if !download_root.is_dir() {
        return Err(anyhow!("Download root does not exist"));
    }

    let base = kvui_song_destination_dir(
        download_root,
        artist,
        title,
    )?;

    let artist_dir = base
        .parent()
        .ok_or_else(|| anyhow!("Missing artist directory"))?;

    kvui_check_destination_directory(artist_dir)?;

    if !kvui_check_destination_directory(&base)? {
        return Ok(base);
    }

    if kvui_song_folder_matches_source(&base, song_url)? {
        return Ok(base);
    }

    let alternate_name = kvui_collision_song_folder(title, song_url)?;
    let alternate = artist_dir.join(alternate_name);

    if !kvui_check_destination_directory(&alternate)? {
        return Ok(alternate);
    }

    if kvui_song_folder_matches_source(&alternate, song_url)? {
        return Ok(alternate);
    }

    Err(anyhow!(
        "Song destination collision cannot be resolved safely"
    ))
}

#[cfg(test)]
mod artist_song_collision_tests {
    use super::{
        kvui_select_song_destination,
        KVUI_SONG_SOURCE_FILE,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_root() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let root = std::env::temp_dir().join(format!(
            "kvui-collision-test-{}-{stamp}",
            std::process::id()
        ));

        fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn selects_clean_artist_song_path() {
        let root = temporary_root();

        let result = kvui_select_song_destination(
            &root, "Eagles", "Hotel California", "https://example.test/1"
        ).unwrap();

        assert_eq!(
            result,
            root.join("Eagles").join("Hotel California")
        );

        fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn reuses_folder_with_matching_source() {
        let root = temporary_root();
        let folder = root.join("Eagles").join("Hotel California");

        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(KVUI_SONG_SOURCE_FILE),
            "https://example.test/1",
        ).unwrap();

        let result = kvui_select_song_destination(
            &root, "Eagles", "Hotel California", "https://example.test/1"
        ).unwrap();

        assert_eq!(result, folder);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn protects_unmarked_existing_folder() {
        let root = temporary_root();
        let folder = root.join("Eagles").join("Hotel California");

        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("Existing.mp3"), b"untouched").unwrap();

        let result = kvui_select_song_destination(
            &root, "Eagles", "Hotel California", "https://example.test/1"
        ).unwrap();

        assert_ne!(result, folder);
        assert_eq!(
            fs::read(folder.join("Existing.mp3")).unwrap(),
            b"untouched"
        );

        assert!(result
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("[KV-"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn separates_different_recordings() {
        let root = temporary_root();
        let folder = root.join("Eagles").join("Hotel California");

        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(KVUI_SONG_SOURCE_FILE),
            "https://example.test/old",
        ).unwrap();

        let result = kvui_select_song_destination(
            &root, "Eagles", "Hotel California", "https://example.test/new"
        ).unwrap();

        assert_ne!(result, folder);

        // An occupied alternative without a matching marker
        // must cause an error rather than overwrite anything.
        fs::create_dir(&result).unwrap();

        assert!(kvui_select_song_destination(
            &root, "Eagles", "Hotel California", "https://example.test/new"
        ).is_err());

        fs::write(
            result.join(KVUI_SONG_SOURCE_FILE),
            "https://example.test/new",
        ).unwrap();

        assert_eq!(
            kvui_select_song_destination(
                &root, "Eagles", "Hotel California", "https://example.test/new"
            ).unwrap(),
            result
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rejects_artist_path_that_is_a_file() {
        let root = temporary_root();
        fs::write(root.join("Eagles"), b"existing file").unwrap();

        assert!(kvui_select_song_destination(
            &root, "Eagles", "Hotel California", "https://example.test/1"
        ).is_err());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rejects_missing_source_url() {
        let root = temporary_root();

        assert!(kvui_select_song_destination(
            &root, "Eagles", "Hotel California", ""
        ).is_err());

        fs::remove_dir(&root).unwrap();
    }
}


/// Prepare a previously selected Artist/Song destination.
///
/// This function must be called only after staged tracks have been
/// downloaded and verified. It does not move or replace MP3 files.
///
/// The source marker prevents another recording from silently
/// sharing the same song folder.
#[allow(dead_code)]
fn kvui_prepare_song_destination(
    download_root: &Path,
    artist: &str,
    title: &str,
    song_url: &str,
    expected_destination: &Path,
) -> Result<()> {
    // The destination must still be the correct choice.
    // A folder could have appeared since initial selection.
    let current_selection = kvui_select_song_destination(
        download_root,
        artist,
        title,
        song_url,
    )?;

    if current_selection.as_path() != expected_destination {
        return Err(anyhow!(
            "Song destination changed before publishing; refusing to overwrite"
        ));
    }

    // Never create folders through an unsafe download root.
    if !kvui_check_destination_directory(download_root)? {
        return Err(anyhow!("Download root does not exist"));
    }

    let artist_dir = expected_destination
        .parent()
        .ok_or_else(|| anyhow!("Song destination has no parent"))?;

    // Create the artist directory without recursively traversing
    // unexpected intermediate directories.
    if !kvui_check_destination_directory(artist_dir)? {
        std::fs::create_dir(artist_dir)?;
    }

    if !kvui_check_destination_directory(artist_dir)? {
        return Err(anyhow!("Artist directory is unavailable"));
    }

    // A new song folder receives a source marker.
    // An existing folder must already have a matching marker.
    if !kvui_check_destination_directory(expected_destination)? {
        std::fs::create_dir(expected_destination)?;

        if !kvui_check_destination_directory(expected_destination)? {
            return Err(anyhow!("Song directory is unavailable"));
        }

        let marker_path = expected_destination
            .join(KVUI_SONG_SOURCE_FILE);

        // create_new prevents accidentally replacing an existing marker.
        let mut marker = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker_path)?;

        std::io::Write::write_all(
            &mut marker,
            song_url.trim().as_bytes(),
        )?;

        marker.sync_all()?;
    }

    // Final ownership verification before file publishing.
    if !kvui_song_folder_matches_source(
        expected_destination,
        song_url,
    )? {
        return Err(anyhow!(
            "Song folder is not assigned to this recording"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod artist_song_preparation_tests {
    use super::{
        kvui_prepare_song_destination,
        kvui_select_song_destination,
        KVUI_SONG_SOURCE_FILE,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SOURCE: &str = "https://example.test/recording/123";

    fn temporary_root() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let root = std::env::temp_dir().join(format!(
            "kvui-preparation-test-{}-{stamp}",
            std::process::id()
        ));

        fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn creates_artist_song_folders_and_marker() {
        let root = temporary_root();

        let destination = kvui_select_song_destination(
            &root,
            "Eagles",
            "Hotel California",
            SOURCE,
        ).unwrap();

        kvui_prepare_song_destination(
            &root,
            "Eagles",
            "Hotel California",
            SOURCE,
            &destination,
        ).unwrap();

        assert!(destination.is_dir());

        assert_eq!(
            fs::read_to_string(
                destination.join(KVUI_SONG_SOURCE_FILE)
            ).unwrap(),
            SOURCE
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reuses_matching_folder_without_touching_files() {
        let root = temporary_root();

        let destination = kvui_select_song_destination(
            &root,
            "Eagles",
            "Hotel California",
            SOURCE,
        ).unwrap();

        kvui_prepare_song_destination(
            &root, "Eagles", "Hotel California",
            SOURCE, &destination,
        ).unwrap();

        fs::write(
            destination.join("Existing.mp3"),
            b"original contents",
        ).unwrap();

        kvui_prepare_song_destination(
            &root, "Eagles", "Hotel California",
            SOURCE, &destination,
        ).unwrap();

        assert_eq!(
            fs::read(destination.join("Existing.mp3")).unwrap(),
            b"original contents"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preserves_unmarked_folder_and_creates_alternative() {
        let root = temporary_root();

        let original = root
            .join("Eagles")
            .join("Hotel California");

        fs::create_dir_all(&original).unwrap();
        fs::write(
            original.join("Existing.mp3"),
            b"do not replace",
        ).unwrap();

        let destination = kvui_select_song_destination(
            &root,
            "Eagles",
            "Hotel California",
            SOURCE,
        ).unwrap();

        assert_ne!(destination, original);

        kvui_prepare_song_destination(
            &root, "Eagles", "Hotel California",
            SOURCE, &destination,
        ).unwrap();

        assert!(destination.is_dir());

        assert_eq!(
            fs::read(original.join("Existing.mp3")).unwrap(),
            b"do not replace"
        );

        assert_eq!(
            fs::read_to_string(
                destination.join(KVUI_SONG_SOURCE_FILE)
            ).unwrap(),
            SOURCE
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_destination_changed_after_selection() {
        let root = temporary_root();

        let destination = kvui_select_song_destination(
            &root,
            "Eagles",
            "Hotel California",
            SOURCE,
        ).unwrap();

        // Simulate a different process occupying the selected path.
        fs::create_dir_all(&destination).unwrap();

        fs::write(
            destination.join(KVUI_SONG_SOURCE_FILE),
            "https://example.test/another-recording",
        ).unwrap();

        assert!(kvui_prepare_song_destination(
            &root, "Eagles", "Hotel California",
            SOURCE, &destination,
        ).is_err());

        assert_eq!(
            fs::read_to_string(
                destination.join(KVUI_SONG_SOURCE_FILE)
            ).unwrap(),
            "https://example.test/another-recording"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_artist_path_occupied_by_file() {
        let root = temporary_root();

        fs::write(
            root.join("Eagles"),
            b"existing file",
        ).unwrap();

        let destination = root
            .join("Eagles")
            .join("Hotel California");

        assert!(kvui_prepare_song_destination(
            &root, "Eagles", "Hotel California",
            SOURCE, &destination,
        ).is_err());

        fs::remove_dir_all(root).unwrap();
    }
}


#[cfg(test)]
mod artist_song_publish_integration_tests {
    use super::{
        kvui_prepare_song_destination,
        kvui_select_song_destination,
        KVUI_SONG_SOURCE_FILE,
    };
    use kv_core::tasks::download_song::{
        create_download_staging_dir,
        publish_staged_file,
        ExistingFilePolicy,
        PublishOutcome,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn verified_file_publishes_into_artist_song_folder() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let root = std::env::temp_dir().join(format!(
            "kvui-publish-integration-{}-{stamp}",
            std::process::id()
        ));

        fs::create_dir(&root).unwrap();

        let url = "https://example.test/recording/123";

        // Staging remains directly under the download root.
        let staging = create_download_staging_dir(&root).unwrap();
        let staged_file = staging.join("Bass.mp3");

        fs::write(&staged_file, b"test MP3 data").unwrap();

        let destination = kvui_select_song_destination(
            &root,
            "Eagles",
            "Hotel California",
            url,
        ).unwrap();

        kvui_prepare_song_destination(
            &root,
            "Eagles",
            "Hotel California",
            url,
            &destination,
        ).unwrap();

        let outcome = publish_staged_file(
            &staged_file,
            &destination,
            ExistingFilePolicy::Keep,
        ).unwrap();

        assert_eq!(outcome, PublishOutcome::Created);

        assert_eq!(
            fs::read(destination.join("Bass.mp3")).unwrap(),
            b"test MP3 data"
        );

        assert_eq!(
            fs::read_to_string(
                destination.join(KVUI_SONG_SOURCE_FILE)
            ).unwrap(),
            url
        );

        assert!(!root.join("Bass.mp3").exists());
        assert!(!staged_file.exists());

        fs::remove_dir_all(&root).unwrap();
    }
}


#[cfg(test)]
mod keyed_song_folder_tests {
    use super::kvui_keyed_song_folder_name;

    #[test]
    fn original_key_keeps_song_title() {
        assert_eq!(
            kvui_keyed_song_folder_name("Hotel California", 0).unwrap(),
            "Hotel California"
        );
    }

    #[test]
    fn negative_shifts_get_minus_suffix() {
        for shift in -4..=-1 {
            assert_eq!(
                kvui_keyed_song_folder_name("Hotel California", shift).unwrap(),
                format!("Hotel California{shift}")
            );
        }
    }

    #[test]
    fn positive_shifts_get_plus_suffix() {
        for shift in 1..=4 {
            assert_eq!(
                kvui_keyed_song_folder_name("Hotel California", shift).unwrap(),
                format!("Hotel California+{shift}")
            );
        }
    }

    #[test]
    fn sanitizes_song_title_before_appending_key() {
        assert_eq!(
            kvui_keyed_song_folder_name("Who: Me?", -2).unwrap(),
            "Who_ Me_-2"
        );
    }

    #[test]
    fn preserves_folder_length_limit() {
        let title = "A".repeat(100);

        let result =
            kvui_keyed_song_folder_name(&title, -2).unwrap();

        assert_eq!(result.chars().count(), 100);
        assert!(result.ends_with("-2"));
    }

    #[test]
    fn rejects_invalid_titles_and_key_adjustments() {
        assert!(kvui_keyed_song_folder_name("", -2).is_err());
        assert!(kvui_keyed_song_folder_name("...", 2).is_err());
        assert!(kvui_keyed_song_folder_name("Song", -5).is_err());
        assert!(kvui_keyed_song_folder_name("Song", 5).is_err());
    }
}

/// Select a key-specific destination without changing files.
#[allow(dead_code)]
fn kvui_select_keyed_song_destination(
    root: &Path,
    artist: &str,
    title: &str,
    url: &str,
    transpose: i8,
) -> Result<PathBuf> {
    if transpose == 0 {
        return kvui_select_song_destination(
            root, artist, title, url
        );
    }

    let keyed_title =
        kvui_keyed_song_folder_name(title, transpose)?;

    kvui_select_song_destination(
        root, artist, &keyed_title, url
    )
}

/// Prepare the selected destination after staging verification.
#[allow(dead_code)]
fn kvui_prepare_keyed_song_destination(
    root: &Path,
    artist: &str,
    title: &str,
    url: &str,
    transpose: i8,
    expected: &Path,
) -> Result<()> {
    if transpose == 0 {
        return kvui_prepare_song_destination(
            root, artist, title, url, expected
        );
    }

    let keyed_title =
        kvui_keyed_song_folder_name(title, transpose)?;

    kvui_prepare_song_destination(
        root, artist, &keyed_title, url, expected
    )
}

#[cfg(test)]
mod keyed_song_destination_tests {
    use super::{
        kvui_select_keyed_song_destination,
        kvui_prepare_keyed_song_destination,
        KVUI_SONG_SOURCE_FILE,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    const URL: &str = "https://example.test/recording/123";

    // Counter prevents collisions between parallel tests.
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

    fn temporary_root() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let sequence =
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed);

        let root = std::env::temp_dir().join(format!(
            "kvui-c20-routing-{}-{stamp}-{sequence}",
            std::process::id()
        ));

        fs::create_dir(&root).unwrap();
        root
    }

    fn select(root: &PathBuf, shift: i8) -> PathBuf {
        kvui_select_keyed_song_destination(
            root, "Eagles", "Hotel California", URL, shift
        )
        .unwrap()
    }

    #[test]
    fn selects_distinct_folders_for_each_key() {
        let root = temporary_root();

        let original = select(&root, 0);
        let minus_two = select(&root, -2);
        let plus_two = select(&root, 2);

        assert_eq!(
            original,
            root.join("Eagles").join("Hotel California")
        );
        assert_eq!(
            minus_two,
            root.join("Eagles").join("Hotel California-2")
        );
        assert_eq!(
            plus_two,
            root.join("Eagles").join("Hotel California+2")
        );

        assert!(!original.exists());
        assert!(!minus_two.exists());
        assert!(!plus_two.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepares_and_reuses_matching_key_folder() {
        let root = temporary_root();
        let destination = select(&root, -2);

        kvui_prepare_keyed_song_destination(
            &root, "Eagles", "Hotel California",
            URL, -2, &destination
        )
        .unwrap();

        assert!(destination.is_dir());

        assert_eq!(
            fs::read_to_string(
                destination.join(KVUI_SONG_SOURCE_FILE)
            )
            .unwrap(),
            URL
        );

        assert_eq!(select(&root, -2), destination);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn protects_unmarked_key_folder() {
        let root = temporary_root();

        let occupied = root
            .join("Eagles")
            .join("Hotel California-2");

        fs::create_dir_all(&occupied).unwrap();

        fs::write(
            occupied.join("Existing.mp3"),
            b"preserve this file"
        )
        .unwrap();

        let selected = select(&root, -2);

        assert_ne!(selected, occupied);

        assert!(
            selected
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("[KV-")
        );

        assert_eq!(
            fs::read(occupied.join("Existing.mp3")).unwrap(),
            b"preserve this file"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_destination_changed_before_publishing() {
        let root = temporary_root();
        let destination = select(&root, -2);

        fs::create_dir_all(&destination).unwrap();

        fs::write(
            destination.join(KVUI_SONG_SOURCE_FILE),
            "https://example.test/other-recording"
        )
        .unwrap();

        assert!(
            kvui_prepare_keyed_song_destination(
                &root, "Eagles", "Hotel California",
                URL, -2, &destination
            )
            .is_err()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unsupported_transposition() {
        let root = temporary_root();

        assert!(
            kvui_select_keyed_song_destination(
                &root, "Eagles", "Hotel California", URL, -5
            )
            .is_err()
        );

        assert!(
            kvui_select_keyed_song_destination(
                &root, "Eagles", "Hotel California", URL, 5
            )
            .is_err()
        );

        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod stateless_download_ui_tests {
    use super::*;

    #[test]
    fn intro_click_defaults_off_and_legacy_progress_menu_is_absent() {
        let app = App::new(Arc::new(Mutex::new(LogBuffer::new(20))));
        assert!(!app.intro_click);
        assert!(!app.menu_items.iter().any(|item| item.contains("Progress")));
    }

    #[test]
    fn current_run_report_counts_only_selected_track_statuses() {
        let tracks = [TrackStatus::Done, TrackStatus::Failed, TrackStatus::Pending,
            TrackStatus::Downloading];
        let items = tracks.iter().enumerate().map(|(index, status)| TrackStatusItem {
            name: format!("Track {index}"), status: status.clone(), attempt: 0,
        }).collect::<Vec<_>>();
        assert_eq!(count_current_download_results(&items), (1, 1, 2));
    }
}
