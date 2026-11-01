mod epub;
mod library;
mod progress;
mod reader;
mod speech;

use std::{env, path::PathBuf, process};

use anyhow::{Result, bail};
use progress::{Position, Store};

fn main() {
    if let Err(error) = run() {
        if error.downcast_ref::<library::Quit>().is_some() {
            return;
        }
        eprintln!("nomnom: {error:#}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let argument = env::args_os().nth(1).map(PathBuf::from);
    if env::args_os().nth(2).is_some() {
        bail!("usage: nomnom [BOOK.epub]");
    }
    let mut selection = match argument {
        Some(path) => {
            if !path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
            {
                bail!("expected an EPUB file");
            }
            library::direct(path)?
        }
        None => library::select(None)?,
    };

    while let Some(selected) = selection {
        let book = epub::Epub::open(&selected.path)?;
        let title = book.metadata().title.clone();
        let mut state = Store::open()?.load(&selected.path)?.unwrap_or_default();
        if selected.restart {
            state.position = Position::default();
            state.completed = false;
            state.percent = 0;
        }
        match reader::open(&selected.path, &title, book, state)? {
            reader::Exit::Quit => return Ok(()),
            reader::Exit::Library => selection = library::select(Some(&selected))?,
        }
    }
    Ok(())
}
