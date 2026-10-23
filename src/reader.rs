use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use anyhow::Result;
use gtk::{
    Application, ApplicationWindow, EventControllerKey, Label, ListBox, ListBoxRow, Orientation,
    PolicyType, ScrolledWindow, gdk, glib, prelude::*,
};

use crate::{
    epub::{BlockKind, Chapter},
    progress::{FocusLevel, Position, ReadingState, Store},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Exit {
    Library,
    Quit,
}

#[derive(Clone)]
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
    chapters: Vec<Chapter>,
    initial: ReadingState,
) -> Result<Exit> {
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
            chapters.clone(),
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
    chapters: Vec<Chapter>,
    initial: ReadingState,
    outcome: Rc<Cell<Exit>>,
) {
    install_css();
    let state = Rc::new(RefCell::new(initial));
    let passages = Rc::new(RefCell::new(make_passages(&chapters, initial.focus)));
    let current = Rc::new(Cell::new(find_passage(
        &passages.borrow(),
        initial.position,
    )));
    let animation_generation = Rc::new(Cell::new(0_u64));

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
        current.get(),
        false,
        Rc::clone(&animation_generation),
        animation_generation.get(),
    );

    let keys = EventControllerKey::new();
    {
        let list = list.clone();
        let scroller = scroller.clone();
        let status = status.clone();
        let window = window.downgrade();
        let passages = Rc::clone(&passages);
        let current = Rc::clone(&current);
        let animation_generation = Rc::clone(&animation_generation);
        let state = Rc::clone(&state);
        let chapters = chapters.clone();
        let outcome = Rc::clone(&outcome);
        let path = path.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let mut rebuild = false;
            let previous = current.get();
            let mut next = previous;
            match key {
                gdk::Key::q => {
                    outcome.set(Exit::Quit);
                    save(&path, &passages.borrow(), current.get(), &state.borrow());
                    if let Some(w) = window.upgrade() {
                        w.close();
                    }
                    return glib::Propagation::Stop;
                }
                gdk::Key::Escape => {
                    outcome.set(Exit::Library);
                    save(&path, &passages.borrow(), current.get(), &state.borrow());
                    if let Some(w) = window.upgrade() {
                        w.close();
                    }
                    return glib::Propagation::Stop;
                }
                gdk::Key::j | gdk::Key::Down => {
                    next = (next + 1).min(passages.borrow().len().saturating_sub(1))
                }
                gdk::Key::k | gdk::Key::Up => next = next.saturating_sub(1),
                gdk::Key::l | gdk::Key::Right => {
                    next = chapter_jump(&passages.borrow(), next, true)
                }
                gdk::Key::h | gdk::Key::Left => {
                    next = chapter_jump(&passages.borrow(), next, false)
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
                    *passages.borrow_mut() = make_passages(&chapters, focus);
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
                _ => return glib::Propagation::Proceed,
            }
            current.set(next);
            let generation = animation_generation.get().wrapping_add(1);
            animation_generation.set(generation);
            if rebuild {
                populate(&list, &passages.borrow(), next, state.borrow().font_size);
            } else if next != previous {
                animate_transition(
                    &list,
                    previous,
                    next,
                    state.borrow().font_size,
                    Rc::clone(&animation_generation),
                    generation,
                );
            } else {
                style_rows(&list, next, state.borrow().font_size);
            }
            center_current(
                &list,
                &scroller,
                next,
                !rebuild && next != previous,
                Rc::clone(&animation_generation),
                generation,
            );
            update_status(&status, &passages.borrow(), next, state.borrow().focus);
            save(&path, &passages.borrow(), next, &state.borrow());
            glib::Propagation::Stop
        });
    }
    window.add_controller(keys);
    window.present();
}

fn make_passages(chapters: &[Chapter], level: FocusLevel) -> Vec<Passage> {
    let mut passages = Vec::new();
    for (chapter_index, chapter) in chapters.iter().enumerate() {
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
    for passage in passages {
        let label = Label::new(Some(&passage.text));
        label.set_wrap(true);
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
    style_rows(list, active, font_size);
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
fn animate_transition(
    list: &ListBox,
    previous: usize,
    next: usize,
    font_size: i32,
    animation_generation: Rc<Cell<u64>>,
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
    glib::timeout_add_local(Duration::from_millis(16), move || {
        if animation_generation.get() != generation {
            return glib::ControlFlow::Break;
        }
        let progress = (started.elapsed().as_secs_f64() / 0.38).clamp(0.0, 1.0);
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

fn center_current(
    list: &ListBox,
    scroller: &ScrolledWindow,
    active: usize,
    smooth: bool,
    animation_generation: Rc<Cell<u64>>,
    generation: u64,
) {
    let Some(row) = list.row_at_index(active as i32) else {
        return;
    };
    let adjustment = scroller.vadjustment();
    glib::idle_add_local_once(move || {
        let allocation = row.allocation();
        let centered = f64::from(allocation.y()) + f64::from(allocation.height()) / 2.0
            - adjustment.page_size() / 2.0;
        let maximum = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        let target = centered.clamp(adjustment.lower(), maximum);
        if !smooth {
            adjustment.set_value(target);
            return;
        }
        let start = adjustment.value();
        let distance = target - start;
        let started = std::time::Instant::now();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            if animation_generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            let progress = (started.elapsed().as_secs_f64() / 0.34).clamp(0.0, 1.0);
            let eased = if progress < 0.5 {
                4.0 * progress * progress * progress
            } else {
                1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0
            };
            adjustment.set_value(start + distance * eased);
            if progress >= 1.0 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
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
fn chapter_jump(passages: &[Passage], current: usize, forward: bool) -> usize {
    let Some(here) = passages.get(current) else {
        return 0;
    };
    if forward {
        passages
            .iter()
            .position(|p| p.chapter > here.chapter)
            .unwrap_or(current)
    } else {
        passages
            .iter()
            .rposition(|p| p.chapter < here.chapter)
            .map(|i| {
                passages[..=i]
                    .iter()
                    .position(|p| p.chapter == passages[i].chapter)
                    .unwrap_or(i)
            })
            .unwrap_or(current)
    }
}
fn save(path: &Path, passages: &[Passage], current: usize, state: &ReadingState) {
    let mut saved = *state;
    saved.position = passage_position(passages, current);
    saved.completed = current + 1 == passages.len();
    if let Ok(store) = Store::open() {
        if let Err(error) = store.save(path, saved) {
            eprintln!("nomnom: failed to save progress: {error:#}");
        }
    }
}
fn install_css() {
    let css = gtk::CssProvider::new();
    css.load_from_data("window { background: #1d2021; color: #ebdbb2; } .passages { background: #1d2021; } row, row:focus, row:focus-visible, row:selected { outline: none; border: none; box-shadow: none; background: transparent; } .passage { color: #ebdbb2; line-height: 1.45; } .heading { font-weight: 700; } .quote { border-left: 3px solid #928374; padding-left: 20px; } .status { color: #a89984; font-size: 13px; }");
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
    use super::{group_sentences, sentences};
    #[test]
    fn groups_without_crossing_a_paragraph() {
        let values = sentences("One. Two! Three?");
        assert_eq!(group_sentences(&values, 2), ["One. Two!", "Three?"]);
    }
}
