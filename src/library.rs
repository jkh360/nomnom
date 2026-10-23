use std::{
    env, fmt, fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use ratatui_image::{
    Resize, StatefulImage,
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
};

use crate::{
    epub::{Epub, Metadata},
    progress::{ReadingState, Store},
};

const BG: Color = Color::Rgb(29, 32, 33);
const FG: Color = Color::Rgb(235, 219, 178);
const MUTED: Color = Color::Rgb(146, 131, 116);
const ACCENT: Color = Color::Rgb(184, 187, 38);
const YELLOW: Color = Color::Rgb(250, 189, 47);

#[derive(Debug)]
pub struct Quit;

impl fmt::Display for Quit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("quit requested")
    }
}

impl std::error::Error for Quit {}

#[derive(Clone)]
pub struct Selection {
    pub path: PathBuf,
    location: Option<Location>,
    pub restart: bool,
}
#[derive(Clone)]
struct Location {
    directory: PathBuf,
    selected: usize,
}
#[derive(Clone)]
enum Entry {
    Directory(PathBuf),
    LastRead {
        path: PathBuf,
        title: String,
        progress: String,
    },
    Book {
        path: PathBuf,
        title: String,
    },
}
impl Entry {
    fn book(&self) -> Option<&Path> {
        match self {
            Self::LastRead { path, .. } | Self::Book { path, .. } => Some(path),
            Self::Directory(_) => None,
        }
    }
    fn label(&self) -> String {
        match self {
            Self::Directory(path) => format!("▸ {}/", name(path)),
            Self::LastRead {
                title, progress, ..
            } => format!("↳ Last read\n    {title}  ·  {progress}"),
            Self::Book { title, .. } => format!("  {title}"),
        }
    }
}

struct Preview {
    metadata: Metadata,
    chapters: usize,
    state: ReadingState,
    book_percent: usize,
    image: Option<StatefulProtocol>,
}

pub fn select(previous: Option<&Selection>) -> Result<Option<Selection>> {
    ensure_terminal()?;
    let configured = default_directory().filter(|p| p.is_dir());
    let mut directory = previous
        .and_then(|s| s.location.as_ref())
        .map(|l| l.directory.clone())
        .or(configured)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_else(|| ".".into())));
    let mut selected = previous
        .and_then(|s| s.location.as_ref())
        .map_or(0, |l| l.selected);
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((10, 20)));
    if env::var("TERM").is_ok_and(|term| term.contains("ghostty")) {
        picker.set_protocol_type(ProtocolType::Kitty);
    }
    let mut terminal = ratatui::init();
    let result = (|| {
        let mut notice = None;
        let mut preview_path = None;
        let mut preview = None;
        loop {
            let entries = entries(&directory)?;
            selected = selected.min(entries.len().saturating_sub(1));
            let target = entries
                .get(selected)
                .and_then(Entry::book)
                .map(Path::to_owned);
            if target != preview_path {
                preview = target
                    .as_deref()
                    .and_then(|path| load_preview(path, &mut picker).ok());
                preview_path = target;
            }
            terminal.draw(|frame| {
                draw_library(
                    frame,
                    &directory,
                    &entries,
                    selected,
                    notice.as_deref(),
                    preview.as_mut(),
                )
            })?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') => return Ok(None),
                KeyCode::Char('j') | KeyCode::Down => {
                    selected = (selected + 1).min(entries.len().saturating_sub(1))
                }
                KeyCode::Char('k') | KeyCode::Up => selected = selected.saturating_sub(1),
                KeyCode::Char('d') => match set_default_directory(&directory) {
                    Ok(()) => notice = Some(format!("Library folder: {}", directory.display())),
                    Err(e) => notice = Some(e.to_string()),
                },
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace | KeyCode::Esc => {
                    if let Some(parent) = directory.parent() {
                        directory = parent.to_owned();
                        selected = 0;
                    }
                }
                KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                    if let Some(entry) = entries.get(selected) {
                        match entry {
                            Entry::Directory(path) => {
                                directory = path.clone();
                                selected = 0;
                            }
                            Entry::LastRead { path, .. } | Entry::Book { path, .. } => {
                                let selection = details(
                                    &mut terminal,
                                    &mut picker,
                                    path.clone(),
                                    Location {
                                        directory: directory.clone(),
                                        selected,
                                    },
                                )?;
                                if selection.is_some() {
                                    return Ok(selection);
                                }
                                preview_path = None;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    })();
    ratatui::restore();
    result
}

pub fn direct(path: PathBuf) -> Result<Option<Selection>> {
    ensure_terminal()?;
    let parent = path.parent().unwrap_or(Path::new(".")).to_owned();
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((10, 20)));
    if env::var("TERM").is_ok_and(|term| term.contains("ghostty")) {
        picker.set_protocol_type(ProtocolType::Kitty);
    }
    let mut terminal = ratatui::init();
    let result = details(
        &mut terminal,
        &mut picker,
        path,
        Location {
            directory: parent,
            selected: 0,
        },
    );
    ratatui::restore();
    result
}

fn details(
    terminal: &mut DefaultTerminal,
    picker: &mut Picker,
    path: PathBuf,
    location: Location,
) -> Result<Option<Selection>> {
    let mut preview = load_preview(&path, picker)?;
    let has_progress = preview.state.position.chapter != 0
        || preview.state.position.block != 0
        || preview.state.position.segment != 0;
    let mut selected = 0;
    loop {
        terminal
            .draw(|frame| draw_book_selector(frame, &path, &mut preview, selected, has_progress))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') => bail!(Quit),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace | KeyCode::Esc => {
                return Ok(None);
            }
            KeyCode::Char('j') | KeyCode::Down => selected = 1,
            KeyCode::Char('k') | KeyCode::Up => selected = 0,
            KeyCode::Char('r') => {
                return Ok(Some(Selection {
                    path,
                    location: Some(location),
                    restart: true,
                }));
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                return Ok(Some(Selection {
                    path,
                    location: Some(location),
                    restart: selected == 1,
                }));
            }
            _ => {}
        }
    }
}

fn entries(directory: &Path) -> Result<Vec<Entry>> {
    let mut directories = Vec::new();
    let mut books = Vec::new();
    for item in fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let item = item?;
        let path = item.path();
        if item.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if item.file_type()?.is_dir() {
            directories.push(path);
        } else if is_epub(&path) {
            books.push(path);
        }
    }
    directories
        .sort_by(|a, b| natord::compare_ignore_case(&a.to_string_lossy(), &b.to_string_lossy()));
    books.sort_by(|a, b| natord::compare_ignore_case(&a.to_string_lossy(), &b.to_string_lossy()));

    let mut values = directories
        .into_iter()
        .map(Entry::Directory)
        .collect::<Vec<_>>();
    if let Some((path, state)) = Store::open()?.most_recent(&books)? {
        let (title, chapters) = book_identity(&path);
        let book_percent = book_percent(&path, state).unwrap_or(0);
        values.push(Entry::LastRead {
            path,
            title,
            progress: progress_label(state, chapters, book_percent),
        });
    }
    values.extend(books.into_iter().map(|path| {
        let (title, _) = book_identity(&path);
        Entry::Book { path, title }
    }));
    Ok(values)
}

fn book_identity(path: &Path) -> (String, usize) {
    Epub::open(path)
        .map(|book| (book.metadata().title.clone(), book.chapter_count()))
        .unwrap_or_else(|_| (name(path), 0))
}

fn progress_label(state: ReadingState, chapters: usize, book_percent: usize) -> String {
    let current = state
        .position
        .chapter
        .saturating_add(1)
        .min(chapters.max(1));
    format!("chapter {current}/{chapters}  ·  {book_percent}% of book")
}

fn book_percent(path: &Path, state: ReadingState) -> Result<usize> {
    if state.completed {
        return Ok(100);
    }
    let mut book = Epub::open(path)?;
    let chapters = book.chapters()?;
    let total_blocks = chapters
        .iter()
        .map(|chapter| chapter.blocks.len())
        .sum::<usize>();
    if total_blocks == 0 {
        return Ok(0);
    }
    let completed_chapters = chapters
        .iter()
        .take(state.position.chapter.min(chapters.len()))
        .map(|chapter| chapter.blocks.len())
        .sum::<usize>();
    let completed_in_chapter = chapters
        .get(state.position.chapter)
        .map_or(0, |chapter| state.position.block.min(chapter.blocks.len()));
    Ok(
        ((completed_chapters + completed_in_chapter) as f64 / total_blocks as f64 * 100.0).round()
            as usize,
    )
}

fn load_preview(path: &Path, picker: &mut Picker) -> Result<Preview> {
    let mut book = Epub::open(path)?;
    let metadata = book.metadata().clone();
    let chapters = book.chapter_count();
    let state = Store::open()?.load(path)?.unwrap_or_default();
    let image = book
        .cover()
        .ok()
        .flatten()
        .map(|cover| picker.new_resize_protocol(cover.thumbnail(1000, 1400)));
    let book_percent = book_percent(path, state).unwrap_or(0);
    Ok(Preview {
        metadata,
        chapters,
        state,
        book_percent,
        image,
    })
}

fn draw_library(
    frame: &mut Frame,
    directory: &Path,
    entries: &[Entry],
    selected: usize,
    notice: Option<&str>,
    preview: Option<&mut Preview>,
) {
    frame.render_widget(
        Block::default().style(Style::default().bg(BG)),
        frame.area(),
    );
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "  NOMNOM  ",
                Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
            ),
            Span::styled(directory.display().to_string(), Style::default().fg(FG)),
        ])),
        areas[0],
    );
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(areas[1]);
    let items = entries
        .iter()
        .map(|entry| {
            ListItem::new(entry.label()).style(Style::default().fg(
                if matches!(entry, Entry::Directory(_)) {
                    ACCENT
                } else if matches!(entry, Entry::LastRead { .. }) {
                    YELLOW
                } else {
                    FG
                },
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected((!items.is_empty()).then_some(selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(" Library ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(MUTED)),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(60, 56, 54))
                    .fg(YELLOW)
                    .add_modifier(Modifier::BOLD),
            ),
        columns[0],
        &mut state,
    );
    draw_preview(frame, columns[1], preview);
    frame.render_widget(
        Paragraph::new(
            notice.unwrap_or("j/k move  ·  l/Enter open  ·  h back  ·  d set library  ·  q quit"),
        )
        .style(Style::default().fg(MUTED)),
        areas[2],
    );
}

fn draw_preview(frame: &mut Frame, area: Rect, preview: Option<&mut Preview>) {
    let block = Block::default()
        .title(" Preview ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(MUTED));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(preview) = preview else {
        frame.render_widget(
            Paragraph::new("Select a book to see its cover and reading progress.")
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    };
    let title_height = 4.min(inner.height);
    let title_area = Rect {
        height: title_height,
        ..inner
    };
    let author = if preview.metadata.authors.is_empty() {
        "Unknown author".to_owned()
    } else {
        preview.metadata.authors.join(", ")
    };
    let series = preview
        .metadata
        .series
        .as_ref()
        .map(|name| {
            format!(
                "{name} {}",
                preview.metadata.series_index.as_deref().unwrap_or("")
            )
            .trim()
            .to_owned()
        })
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                preview.metadata.title.clone(),
                Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(author, Style::default().fg(FG))),
            Line::from(Span::styled(series, Style::default().fg(MUTED))),
            Line::from(Span::styled(
                progress_label(preview.state, preview.chapters, preview.book_percent),
                Style::default().fg(ACCENT),
            )),
        ])
        .wrap(Wrap { trim: true }),
        title_area,
    );
    if let Some(image) = preview.image.as_mut() {
        let image_area = Rect {
            x: inner.x,
            y: inner.y.saturating_add(title_height + 1),
            width: inner.width,
            height: inner.height.saturating_sub(title_height + 1),
        };
        if image_area.width > 0 && image_area.height > 0 {
            frame.render_stateful_widget(
                StatefulImage::default().resize(Resize::Fit(None)),
                image_area,
                image,
            );
        }
    }
}

fn draw_book_selector(
    frame: &mut Frame,
    path: &Path,
    preview: &mut Preview,
    selected: usize,
    has_progress: bool,
) {
    frame.render_widget(
        Block::default().style(Style::default().bg(BG)),
        frame.area(),
    );
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "  NOMNOM  ",
                Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "{} / {}",
                    path.parent().unwrap_or(Path::new(".")).display(),
                    preview.metadata.title
                ),
                Style::default().fg(FG),
            ),
        ])),
        areas[0],
    );
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(areas[1]);
    let continue_label = if has_progress {
        format!(
            "↳ Continue\n    {}",
            progress_label(preview.state, preview.chapters, preview.book_percent)
        )
    } else {
        "  Start reading".to_owned()
    };
    let items = vec![
        ListItem::new(continue_label).style(Style::default().fg(YELLOW)),
        ListItem::new("  Start from beginning").style(Style::default().fg(FG)),
    ];
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(" Book ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(MUTED)),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(60, 56, 54))
                    .fg(YELLOW)
                    .add_modifier(Modifier::BOLD),
            ),
        columns[0],
        &mut state,
    );
    draw_preview(frame, columns[1], Some(preview));
    frame.render_widget(
        Paragraph::new("j/k move  ·  l/Enter open  ·  h/Esc back  ·  q quit")
            .style(Style::default().fg(MUTED)),
        areas[2],
    );
}

fn ensure_terminal() -> Result<()> {
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "NomNom needs an interactive terminal"
    );
    Ok(())
}
fn name(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
fn is_epub(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
}
fn config_path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("XDG_CONFIG_HOME") {
        Ok(PathBuf::from(p).join("nomnom/library-folder"))
    } else {
        Ok(
            PathBuf::from(env::var_os("HOME").context("HOME is not set")?)
                .join(".config/nomnom/library-folder"),
        )
    }
}
fn default_directory() -> Option<PathBuf> {
    fs::read(config_path().ok()?)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .map(PathBuf::from)
}
fn set_default_directory(path: &Path) -> Result<()> {
    let file = config_path()?;
    fs::create_dir_all(file.parent().context("invalid config path")?)?;
    fs::write(file, path.canonicalize()?.to_string_lossy().as_bytes())?;
    Ok(())
}
