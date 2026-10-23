use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Position {
    pub chapter: usize,
    pub block: usize,
    pub segment: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusLevel {
    Sentence,
    Short,
    Medium,
    Paragraph,
}
impl FocusLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sentence => "sentence",
            Self::Short => "short",
            Self::Medium => "medium",
            Self::Paragraph => "paragraph",
        }
    }
    fn parse(value: &str) -> Self {
        match value {
            "sentence" => Self::Sentence,
            "short" => Self::Short,
            "paragraph" => Self::Paragraph,
            _ => Self::Medium,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReadingState {
    pub position: Position,
    pub focus: FocusLevel,
    pub font_size: i32,
    pub completed: bool,
}
impl Default for ReadingState {
    fn default() -> Self {
        Self {
            position: Position::default(),
            focus: FocusLevel::Medium,
            font_size: 20,
            completed: false,
        }
    }
}

pub struct Store {
    connection: Connection,
}
impl Store {
    pub fn open() -> Result<Self> {
        let path = database_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        initialize(&connection)?;
        Ok(Self { connection })
    }
    pub fn load(&self, path: &Path) -> Result<Option<ReadingState>> {
        let key = document_key(path)?;
        self.connection.query_row(
            "SELECT chapter, block, segment, focus_level, font_size, completed FROM progress WHERE path=?1",
            [key], |row| Ok(ReadingState {
                position: Position { chapter: row.get::<_, i64>(0)?.max(0) as usize, block: row.get::<_, i64>(1)?.max(0) as usize, segment: row.get::<_, i64>(2)?.max(0) as usize },
                focus: FocusLevel::parse(&row.get::<_, String>(3)?),
                font_size: row.get::<_, i32>(4)?.clamp(14, 48),
                completed: row.get(5)?,
            })
        ).optional().map_err(Into::into)
    }
    pub fn most_recent(&self, paths: &[PathBuf]) -> Result<Option<(PathBuf, ReadingState)>> {
        let mut statement = self.connection.prepare(
            "SELECT chapter, block, segment, focus_level, font_size, completed, updated_at
             FROM progress WHERE path=?1",
        )?;
        let mut latest = None;
        for path in paths {
            let key = document_key(path)?;
            let row: Option<(ReadingState, i64)> = statement
                .query_row([key], |row| {
                    Ok((
                        ReadingState {
                            position: Position {
                                chapter: row.get::<_, i64>(0)?.max(0) as usize,
                                block: row.get::<_, i64>(1)?.max(0) as usize,
                                segment: row.get::<_, i64>(2)?.max(0) as usize,
                            },
                            focus: FocusLevel::parse(&row.get::<_, String>(3)?),
                            font_size: row.get::<_, i32>(4)?.clamp(14, 48),
                            completed: row.get(5)?,
                        },
                        row.get(6)?,
                    ))
                })
                .optional()?;
            if let Some((state, updated_at)) = row
                && latest
                    .as_ref()
                    .is_none_or(|(_, _, latest_at)| updated_at > *latest_at)
            {
                latest = Some((path.clone(), state, updated_at));
            }
        }
        Ok(latest.map(|(path, state, _)| (path, state)))
    }

    pub fn save(&self, path: &Path, state: ReadingState) -> Result<()> {
        let key = document_key(path)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.connection.execute(
            "INSERT INTO progress(path,chapter,block,segment,focus_level,font_size,completed,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(path) DO UPDATE SET chapter=excluded.chapter,block=excluded.block,segment=excluded.segment,focus_level=excluded.focus_level,font_size=excluded.font_size,completed=excluded.completed,updated_at=excluded.updated_at",
            params![key, state.position.chapter as i64, state.position.block as i64, state.position.segment as i64, state.focus.as_str(), state.font_size, state.completed, timestamp]
        )?;
        Ok(())
    }
}

fn initialize(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS progress(
            path TEXT PRIMARY KEY NOT NULL,
            chapter INTEGER NOT NULL CHECK(chapter>=0),
            block INTEGER NOT NULL CHECK(block>=0),
            segment INTEGER NOT NULL DEFAULT 0 CHECK(segment>=0),
            focus_level TEXT NOT NULL DEFAULT 'medium',
            font_size INTEGER NOT NULL DEFAULT 20,
            completed INTEGER NOT NULL DEFAULT 0 CHECK(completed IN (0,1)),
            updated_at INTEGER NOT NULL
        );",
    )?;
    Ok(())
}
fn database_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("nomnom/library.sqlite3"));
    }
    Ok(
        PathBuf::from(env::var_os("HOME").context("HOME is not set")?)
            .join(".local/state/nomnom/library.sqlite3"),
    )
}
fn document_key(path: &Path) -> Result<String> {
    Ok(path.canonicalize()?.to_string_lossy().into_owned())
}
