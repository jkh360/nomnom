use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    time::Duration,
};

use anyhow::{Result, bail};
use gtk::{
    Application, ApplicationWindow, EventControllerKey, Label, ListBox, ListBoxRow, Orientation,
    PolicyType, ScrolledWindow, gdk, glib, prelude::*,
};

use crate::{
    epub::{BlockKind, Chapter, Epub},
    progress::{FocusLevel, Position, ReadingState, Store},
    speech::Speech,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Exit {
    Library,
    Quit,
}

#[derive(Clone)]
struct LoadedBook {
    document: Rc<RefCell<Epub>>,
    chapter: Chapter,
    chapter_count: usize,
}

struct Passage {
    chapter: usize,
    chapter_title: String,
    block: usize,
    segment: usize,
    text: String,
    kind: BlockKind,
}

pub fn open(
    path: &Path,
    title: &str,
    mut document: Epub,
    mut initial: ReadingState,
) -> Result<Exit> {
    let chapter_count = document.chapter_count();
    let requested_chapter = initial
        .position
        .chapter
        .min(chapter_count.saturating_sub(1));
    let Some((initial_chapter, chapter, _)) = load_readable_chapter(
        &mut document,
        requested_chapter,
        true,
        chapter_count,
        initial.focus,
    )?
    else {
        bail!("EPUB contains no readable text");
    };
    initial.position.chapter = initial_chapter;
    let book = LoadedBook {
        document: Rc::new(RefCell::new(document)),
        chapter,
        chapter_count,
    };
    let outcome = Rc::new(Cell::new(Exit::Quit));
    let requested = Rc::clone(&outcome);
    let path = path.to_owned();
    let title = title.to_owned();
    let application = Application::builder()
        .application_id("dev.nomnom.Reader")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.connect_activate(move |app| {
        build(
            app,
            path.clone(),
            title.clone(),
            LoadedBook {
                document: Rc::clone(&book.document),
                chapter: book.chapter.clone(),
                chapter_count: book.chapter_count,
            },
            initial,
            Rc::clone(&requested),
        )
    });
    application.run_with_args::<&str>(&[]);
    Ok(outcome.get())
}

fn build(
    app: &Application,
    path: PathBuf,
    title: String,
    book: LoadedBook,
    initial: ReadingState,
    outcome: Rc<Cell<Exit>>,
) {
    install_css();
    let LoadedBook {
        document,
        chapter,
        chapter_count,
    } = book;
    let state = Rc::new(RefCell::new(initial));
    let current_chapter = Rc::new(RefCell::new(chapter));
    let passages = Rc::new(RefCell::new(make_passages(
        &current_chapter.borrow(),
        initial.position.chapter,
        initial.focus,
    )));
    let current = Rc::new(Cell::new(find_passage(
        &passages.borrow(),
        initial.position,
    )));
    let transition_generation = Rc::new(Cell::new(0_u64));
    let speech = Rc::new(Speech::new());

    let list = ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("passages");
    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let status = Label::new(None);
    status.add_css_class("status");
    status.set_margin_top(8);
    status.set_margin_bottom(10);
    let layout = gtk::Box::new(Orientation::Vertical, 0);
    layout.append(&scroller);
    layout.append(&status);
    let window = ApplicationWindow::builder()
        .application(app)
        .title(&title)
        .default_width(900)
        .default_height(850)
        .child(&layout)
        .build();

    populate(
        &list,
        &passages.borrow(),
        current.get(),
        state.borrow().font_size,
    );
    update_status(
        &status,
        &passages.borrow(),
        current.get(),
        state.borrow().focus,
    );
    center_current(
        &list,
        &scroller,
        visible_index(&passages.borrow(), current.get()),
    );

    let keys = EventControllerKey::new();
    {
        let list = list.clone();
        let scroller = scroller.clone();
        let status = status.clone();
        let window = window.downgrade();
        let passages = Rc::clone(&passages);
        let current = Rc::clone(&current);
        let transition_generation = Rc::clone(&transition_generation);
        let state = Rc::clone(&state);
        let document = Rc::clone(&document);
        let current_chapter = Rc::clone(&current_chapter);
        let outcome = Rc::clone(&outcome);
        let speech = Rc::clone(&speech);
        let path = path.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let mut rebuild = false;
            let previous = current.get();
            let mut next = previous;
            let active_chapter = passages
                .borrow()
                .first()
                .map_or(0, |passage| passage.chapter);
            let mut target_chapter = None;
            let mut open_at_end = false;
            match key {
                gdk::Key::q => {
                    speech.stop();
                    outcome.set(Exit::Quit);
                    save(
                        &path,
                        &passages.borrow(),
                        current.get(),
                        chapter_count,
                        &state.borrow(),
                    );
                    if let Some(w) = window.upgrade() {
                        w.close();
                    }
                    return glib::Propagation::Stop;
                }
                gdk::Key::Escape => {
                    speech.stop();
                    outcome.set(Exit::Library);
                    save(
                        &path,
                        &passages.borrow(),
                        current.get(),
                        chapter_count,
                        &state.borrow(),
                    );
                    if let Some(w) = window.upgrade() {
                        w.close();
                    }
                    return glib::Propagation::Stop;
                }
                gdk::Key::j | gdk::Key::Down => {
                    if next + 1 < passages.borrow().len() {
                        next += 1;
                    } else if active_chapter + 1 < chapter_count {
                        target_chapter = Some(active_chapter + 1);
                    }
                }
                gdk::Key::k | gdk::Key::Up => {
                    if next > 0 {
                        next -= 1;
                    } else if active_chapter > 0 {
                        target_chapter = Some(active_chapter - 1);
                        open_at_end = true;
                    }
                }
                gdk::Key::l | gdk::Key::Right => {
                    if active_chapter + 1 < chapter_count {
                        target_chapter = Some(active_chapter + 1);
                    }
                }
                gdk::Key::h | gdk::Key::Left => {
                    if active_chapter > 0 {
                        target_chapter = Some(active_chapter - 1);
                    }
                }
                gdk::Key::_1 | gdk::Key::_2 | gdk::Key::_3 | gdk::Key::_4 => {
                    let focus = match key {
                        gdk::Key::_1 => FocusLevel::Sentence,
                        gdk::Key::_2 => FocusLevel::Short,
                        gdk::Key::_4 => FocusLevel::Paragraph,
                        _ => FocusLevel::Medium,
                    };
                    let position = passage_position(&passages.borrow(), next);
                    state.borrow_mut().focus = focus;
                    *passages.borrow_mut() =
                        make_passages(&current_chapter.borrow(), active_chapter, focus);
                    next = find_passage(&passages.borrow(), position);
                    rebuild = true;
                }
                gdk::Key::plus | gdk::Key::equal => {
                    let size = state.borrow().font_size;
                    state.borrow_mut().font_size = (size + 2).min(48);
                    rebuild = true;
                }
                gdk::Key::minus => {
                    let size = state.borrow().font_size;
                    state.borrow_mut().font_size = (size - 2).max(14);
                    rebuild = true;
                }
                gdk::Key::d => {
                    if let Some(parent) = window.upgrade() {
                        show_dictionary(
                            &list,
                            visible_index(&passages.borrow(), current.get()),
                            &parent,
                        );
                    }
                    return glib::Propagation::Stop;
                }
                gdk::Key::s => {
                    let passage = &passages.borrow()[current.get()];
                    let selected =
                        selected_text(&list, visible_index(&passages.borrow(), current.get()));
                    speech.speak(selected.as_deref().unwrap_or(&passage.text));
                    return glib::Propagation::Stop;
                }
                gdk::Key::x => {
                    speech.stop();
                    return glib::Propagation::Stop;
                }
                _ => return glib::Propagation::Proceed,
            }
            speech.stop();
            if let Some(target) = target_chapter {
                let focus = state.borrow().focus;
                match load_readable_chapter(
                    &mut document.borrow_mut(),
                    target,
                    target >= active_chapter,
                    chapter_count,
                    focus,
                ) {
                    Ok(Some((chapter_index, chapter, loaded_passages))) => {
                        *current_chapter.borrow_mut() = chapter;
                        *passages.borrow_mut() = loaded_passages;
                        next = if open_at_end {
                            passages.borrow().len().saturating_sub(1)
                        } else {
                            0
                        };
                        rebuild = true;
                        state.borrow_mut().position.chapter = chapter_index;
                    }
                    Ok(None) => {}
                    Err(error) => eprintln!("nomnom: failed to load chapter: {error:#}"),
                }
            }
            current.set(next);
            let generation = transition_generation.get().wrapping_add(1);
            transition_generation.set(generation);
            if rebuild {
                populate(&list, &passages.borrow(), next, state.borrow().font_size);
            } else if next != previous {
                fade_passages(
                    &list,
                    visible_index(&passages.borrow(), previous),
                    visible_index(&passages.borrow(), next),
                    state.borrow().font_size,
                    Rc::clone(&transition_generation),
                    generation,
                );
            } else {
                style_rows(
                    &list,
                    visible_index(&passages.borrow(), next),
                    state.borrow().font_size,
                );
            }
            center_current(&list, &scroller, visible_index(&passages.borrow(), next));
            update_status(&status, &passages.borrow(), next, state.borrow().focus);
            save(
                &path,
                &passages.borrow(),
                next,
                chapter_count,
                &state.borrow(),
            );
            glib::Propagation::Stop
        });
    }
    window.add_controller(keys);
    window.present();
}

fn make_passages(chapter: &Chapter, chapter_index: usize, level: FocusLevel) -> Vec<Passage> {
    let mut passages = Vec::new();
    for (block_index, block) in chapter.blocks.iter().enumerate() {
        let pieces = if block.kind == BlockKind::Heading || level == FocusLevel::Paragraph {
            vec![block.text.clone()]
        } else {
            group_sentences(
                &sentences(&block.text),
                match level {
                    FocusLevel::Sentence => 1,
                    FocusLevel::Short => 2,
                    FocusLevel::Medium => 4,
                    FocusLevel::Paragraph => usize::MAX,
                },
            )
        };
        for (segment, text) in pieces.into_iter().enumerate() {
            passages.push(Passage {
                chapter: chapter_index,
                chapter_title: chapter.title.clone(),
                block: block_index,
                segment,
                text,
                kind: block.kind,
            });
        }
    }
    passages
}

fn sentences(text: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        if matches!(ch, '.' | '!' | '?' | '…') {
            let end = index + ch.len_utf8();
            let sentence = text[start..end].trim();
            if !sentence.is_empty() {
                output.push(sentence.to_owned());
            }
            start = end;
        }
    }
    let rest = text[start..].trim();
    if !rest.is_empty() {
        output.push(rest.to_owned());
    }
    if output.is_empty() && !text.trim().is_empty() {
        output.push(text.trim().to_owned());
    }
    output
}
fn group_sentences(sentences: &[String], size: usize) -> Vec<String> {
    sentences
        .chunks(size.max(1))
        .map(|chunk| chunk.join(" "))
        .collect()
}

fn populate(list: &ListBox, passages: &[Passage], active: usize, font_size: i32) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let chapter = passages.get(active).map_or(0, |passage| passage.chapter);
    for passage in passages.iter().filter(|passage| passage.chapter == chapter) {
        let label = Label::new(Some(&passage.text));
        label.set_wrap(true);
        label.set_selectable(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_xalign(0.0);
        label.set_max_width_chars(64);
        label.set_margin_start(110);
        label.set_margin_end(110);
        label.set_margin_top(22);
        label.set_margin_bottom(22);
        if passage.kind == BlockKind::Heading {
            label.add_css_class("heading");
        }
        if passage.kind == BlockKind::Quote {
            label.add_css_class("quote");
        }
        let row = ListBoxRow::new();
        row.set_focusable(true);
        row.set_child(Some(&label));
        list.append(&row);
    }
    style_rows(list, visible_index(passages, active), font_size);
}

fn visible_index(passages: &[Passage], active: usize) -> usize {
    let Some(current) = passages.get(active) else {
        return 0;
    };
    passages[..active]
        .iter()
        .filter(|passage| passage.chapter == current.chapter)
        .count()
}
fn style_rows(list: &ListBox, active: usize, font_size: i32) {
    let mut index = 0;
    let mut child = list.first_child();
    while let Some(widget) = child {
        if let Ok(row) = widget.clone().downcast::<ListBoxRow>() {
            row.remove_css_class("active");
            row.remove_css_class("inactive");
            let is_active = index == active;
            row.add_css_class(if is_active { "active" } else { "inactive" });
            row.set_opacity(if is_active { 1.0 } else { 0.18 });
            if let Some(label) = row.child().and_then(|w| w.downcast::<Label>().ok()) {
                label.remove_css_class("current");
                label.remove_css_class("surrounding");
                label.add_css_class("passage");
                label.add_css_class(if index == active {
                    "current"
                } else {
                    "surrounding"
                });
                label.set_markup(&format!(
                    "<span size=\"{}pt\">{}</span>",
                    (font_size - 1).max(12),
                    glib::markup_escape_text(&label.text())
                ));
            }
        }
        child = widget.next_sibling();
        index += 1;
    }
}
fn fade_passages(
    list: &ListBox,
    previous: usize,
    next: usize,
    font_size: i32,
    transition_generation: Rc<Cell<u64>>,
    generation: u64,
) {
    style_rows(list, next, font_size);
    let Some(outgoing) = list.row_at_index(previous as i32) else {
        return;
    };
    let Some(incoming) = list.row_at_index(next as i32) else {
        return;
    };
    outgoing.set_opacity(1.0);
    incoming.set_opacity(0.18);
    let started = std::time::Instant::now();
    glib::timeout_add_local(Duration::from_millis(33), move || {
        if transition_generation.get() != generation {
            return glib::ControlFlow::Break;
        }
        let progress = (started.elapsed().as_secs_f64() / 0.18).clamp(0.0, 1.0);
        let eased = progress * progress * (3.0 - 2.0 * progress);
        outgoing.set_opacity(1.0 - eased * 0.82);
        incoming.set_opacity(0.18 + eased * 0.82);
        if progress >= 1.0 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

fn label_selection(label: &Label) -> Option<String> {
    let (start, end) = label.selection_bounds()?;
    let selected = label
        .text()
        .chars()
        .skip(start.max(0) as usize)
        .take(end.saturating_sub(start) as usize)
        .collect::<String>();
    (!selected.trim().is_empty()).then_some(selected)
}

fn selected_text(list: &ListBox, active: usize) -> Option<String> {
    let label = list
        .row_at_index(active as i32)?
        .child()?
        .downcast::<Label>()
        .ok()?;
    label_selection(&label)
}

fn selected_word(text: &str) -> Option<&str> {
    let word = text
        .trim()
        .trim_matches(|character: char| !character.is_alphanumeric());
    (!word.is_empty()).then_some(word)
}

fn show_dictionary(list: &ListBox, active: usize, parent: &ApplicationWindow) {
    let Some(selected) = selected_text(list, active) else {
        return;
    };
    let Some(word) = selected_word(&selected) else {
        return;
    };
    show_definition(word, parent.upcast_ref());
}

fn show_definition(word: &str, parent: &gtk::Window) {
    let definition = dictionary_definition(word)
        .unwrap_or_else(|| format!("No definition was found for “{word}”."));
    let content = Label::new(Some(&definition));
    content.set_wrap(true);
    content.set_selectable(true);
    content.add_css_class("dictionary-definition");
    content.set_xalign(0.0);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();
    let popup = gtk::Window::builder()
        .title(word)
        .transient_for(parent)
        .modal(true)
        .default_width(560)
        .default_height(360)
        .child(&scroller)
        .build();
    let keys = EventControllerKey::new();
    let weak_popup = popup.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(popup) = weak_popup.upgrade() else {
            return glib::Propagation::Proceed;
        };
        match key {
            gdk::Key::q | gdk::Key::Escape => {
                popup.close();
                glib::Propagation::Stop
            }
            gdk::Key::d => {
                if let Some(selected) = label_selection(&content)
                    && let Some(word) = selected_word(&selected)
                {
                    show_definition(word, &popup);
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    popup.add_controller(keys);
    popup.present();
}

fn dictionary_definition(word: &str) -> Option<String> {
    let providers: [(&str, &[&str]); 3] = [
        ("dict", &[word]),
        ("sdcv", &["--non-interactive", word]),
        ("wn", &[word, "-over"]),
    ];
    providers.into_iter().find_map(|(program, arguments)| {
        let output = Command::new(program).args(arguments).output().ok()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if text.is_empty() {
            None
        } else if program == "wn" {
            Some(clean_wordnet_output(&text))
        } else {
            Some(text)
        }
    })
}

fn clean_wordnet_output(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("Overview of ")
                && !(trimmed.starts_with("The ")
                    && trimmed.contains(" has ")
                    && trimmed.contains(" sense"))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn center_current(list: &ListBox, scroller: &ScrolledWindow, active: usize) {
    let Some(row) = list.row_at_index(active as i32) else {
        return;
    };
    let adjustment = scroller.vadjustment();
    glib::idle_add_local_once(move || {
        let allocation = row.allocation();
        let centered = f64::from(allocation.y()) + f64::from(allocation.height()) / 2.0
            - adjustment.page_size() / 2.0;
        let maximum = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        adjustment.set_value(centered.clamp(adjustment.lower(), maximum));
    });
}
fn update_status(label: &Label, passages: &[Passage], current: usize, focus: FocusLevel) {
    if passages.is_empty() {
        label.set_text("No readable text");
        return;
    }
    let passage = &passages[current];
    let chapter = passage.chapter + 1;
    let percent = ((current + 1) as f64 / passages.len() as f64 * 100.0).round() as usize;
    label.set_text(&format!(
        "Chapter {chapter}: {}  ·  {} / {}  ·  {percent}%  ·  {}",
        passage.chapter_title,
        current + 1,
        passages.len(),
        focus.as_str()
    ));
}
fn passage_position(passages: &[Passage], index: usize) -> Position {
    passages
        .get(index)
        .map(|p| Position {
            chapter: p.chapter,
            block: p.block,
            segment: p.segment,
        })
        .unwrap_or_default()
}
fn find_passage(passages: &[Passage], position: Position) -> usize {
    passages
        .iter()
        .position(|p| {
            p.chapter == position.chapter
                && p.block == position.block
                && p.segment >= position.segment
        })
        .or_else(|| {
            passages
                .iter()
                .position(|p| p.chapter == position.chapter && p.block >= position.block)
        })
        .unwrap_or(0)
}
fn load_readable_chapter(
    document: &mut Epub,
    start: usize,
    forward: bool,
    chapter_count: usize,
    focus: FocusLevel,
) -> Result<Option<(usize, Chapter, Vec<Passage>)>> {
    let mut index = start;
    loop {
        let chapter = document.chapter(index)?;
        let passages = make_passages(&chapter, index, focus);
        if !passages.is_empty() {
            return Ok(Some((index, chapter, passages)));
        }
        if forward {
            if index + 1 >= chapter_count {
                return Ok(None);
            }
            index += 1;
        } else {
            if index == 0 {
                return Ok(None);
            }
            index -= 1;
        }
    }
}
fn save(
    path: &Path,
    passages: &[Passage],
    current: usize,
    chapter_count: usize,
    state: &ReadingState,
) {
    let mut saved = *state;
    saved.position = passage_position(passages, current);
    saved.completed = saved.position.chapter + 1 == chapter_count && current + 1 == passages.len();
    let chapter_fraction = if passages.is_empty() {
        0.0
    } else {
        (current + 1) as f64 / passages.len() as f64
    };
    saved.percent = if chapter_count == 0 {
        0
    } else {
        (((saved.position.chapter as f64 + chapter_fraction) / chapter_count as f64) * 100.0)
            .round() as usize
    };
    if let Ok(store) = Store::open() {
        if let Err(error) = store.save(path, saved) {
            eprintln!("nomnom: failed to save progress: {error:#}");
        }
    }
}
fn install_css() {
    let css = gtk::CssProvider::new();
    css.load_from_data("window { background: #1d2021; color: #ebdbb2; } .passages { background: #1d2021; } row, row:focus, row:focus-visible, row:selected, .passage:focus, .passage:focus-visible { outline: none; border: none; box-shadow: none; background: transparent; } .passage { color: #ebdbb2; line-height: 1.45; } .heading { font-weight: 700; } .quote { border-left: 3px solid #928374; padding-left: 20px; } .dictionary-definition { font-size: 18px; line-height: 1.45; } .status { color: #a89984; font-size: 13px; }");
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{clean_wordnet_output, group_sentences, sentences};

    #[test]
    fn removes_wordnet_overview_headers() {
        let output = clean_wordnet_output(
            "Overview of noun antipathy\n\nThe noun antipathy has 2 senses (first 1 from tagged texts)\n\n1. antipathy -- (a feeling of intense dislike)\n2. antipathy -- (the object of a feeling of intense aversion)",
        );
        assert!(output.starts_with("1. antipathy"));
        assert!(!output.contains("Overview"));
        assert!(!output.contains("has 2 senses"));
    }

    #[test]
    fn groups_without_crossing_a_paragraph() {
        let values = sentences("One. Two! Three?");
        assert_eq!(group_sentences(&values, 2), ["One. Two!", "Three?"]);
    }
}
