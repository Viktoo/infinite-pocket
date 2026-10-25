//! The screens before a world: the title, your worlds, a new world, playing
//! together, sharing, and the settings. One loop, a stack of pages (Back or
//! Esc goes down one), the land of words moving behind them all.

use super::settings::{self as page, Live, Rows, ROW};
use super::title::Backdrop;
use super::{ACCENT, DIM, FAINT, Kind, PANEL, PANEL_EDGE, R, TEXT, Ui, fit, left, note};
use crate::picker::{self, Draft, Edit, World};
use crate::settings::Settings;
use crate::term::{Deco, Face, Label, Screen};
use crate::window::{Ev, Win};
use std::path::PathBuf;
use std::time::Duration;

/// What the screens ended with.
pub enum Choice {
    Open(PathBuf),
    /// A new world: its prompt and difficulty (an index into the levels).
    New(String, usize),
    /// Open a world for others to join (over the internet too, when true).
    Host(PathBuf, bool),
    /// Join someone's world: their address and code.
    Join(String, String),
    Quit,
}

/// Where the screens open.
pub enum Entry {
    Title,
    Worlds,
    /// The new-world screen, on this prompt (or a random one).
    New(Option<String>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum P {
    Title,
    Worlds,
    New,
    Together,
    JoinCode,
    Host,
    Share,
    Delete,
    Settings,
    /// The first time, with no AI key: what it's for, and a place to put it.
    Welcome,
}

enum Sharing {
    Asking,
    Made(PathBuf),
    Failed(String),
}

/// New worlds start at this difficulty (index into the levels: normal).
const NEW_DIFFICULTY: usize = 2;

struct Menus {
    stack: Vec<P>,
    list: Vec<World>,
    sel: usize,
    /// The first card shown (the list scrolls).
    first: usize,
    note: Option<String>,
    settings: Settings,
    settings_page: page::Page,
    draft: Draft,
    difficulty: usize,
    sharing: Sharing,
    listen: Option<crate::net::discover::Listen>,
    found: Vec<crate::net::discover::Found>,
    game: Option<usize>,
    addr: String,
    code: String,
    /// The world to host (an index into the list).
    host: usize,
    ngrok: bool,
    /// A key being checked, and what the last check said (true: it works).
    checking: Option<crossbeam_channel::Receiver<Result<(), String>>>,
    checked: Option<Result<(), String>>,
}

/// Run the screens until a world is chosen (or the window closes), Maple
/// Cove behind them (`pano`, started here if it isn't yet, and left running
/// for the loading screen after).
pub fn menus(win: &mut Win, entry: Entry, note: Option<String>, pano: &mut Option<crate::panorama::Panorama>) -> Choice {
    crate::net::update::ask();
    let (w, h, _) = win.grid();
    let mut scr = Screen::new(w, h, true);
    let mut ui = Ui::new();
    let mut back = Backdrop::new();
    let mut m = Menus {
        stack: vec![P::Title],
        list: picker::worlds(),
        sel: 0,
        first: 0,
        note,
        settings: Settings::load(),
        settings_page: page::Page::default(),
        draft: Draft::rolled(),
        difficulty: NEW_DIFFICULTY,
        sharing: Sharing::Asking,
        listen: None,
        found: Vec::new(),
        game: None,
        addr: String::new(),
        code: String::new(),
        host: 0,
        ngrok: crate::net::ngrok::find().is_some(),
        checking: None,
        checked: None,
    };
    match entry {
        Entry::Title => {}
        Entry::Worlds => m.stack.push(P::Worlds),
        Entry::New(given) => {
            m.stack.push(P::New);
            if let Some(g) = given.as_deref().map(str::trim).filter(|g| !g.is_empty()) {
                m.draft = Draft::given(g);
            }
        }
    }
    // The first time: say what the key is for, and take it.
    if !m.settings.welcomed && !m.settings.has_llm() {
        m.stack.push(P::Welcome);
    }
    ui.focus = Some(0);
    // Maple Cove behind it all, once it has loaded.
    if pano.is_none() {
        *pano = Some(crate::panorama::Panorama::start());
    }
    loop {
        let evs = win.pump(Duration::from_millis(33));
        for e in &evs {
            match e {
                Ev::Close => return Choice::Quit,
                Ev::Dropped(p) => m.dropped(p),
                e => {
                    ui.feed(e);
                }
            }
        }
        let (w, h, g) = win.grid();
        if (scr.w, scr.h) != (w, h) {
            scr.resize(w, h);
        }
        if let Some(p) = pano.as_mut() {
            p.tick((w as f32 * g.cell_w, h as f32 * g.cell_h));
        }
        let frame = pano.as_ref().and_then(|p| p.frame().map(|f| (f, p.shown())));
        if m.listen.is_none() && matches!(m.top(), P::Together | P::JoinCode) {
            m.listen = crate::net::discover::Listen::start();
        }
        if let Some(l) = m.listen.as_mut() {
            m.found = l.hosts();
        }
        // Drawn once with the input, and again if it changed anything.
        for _ in 0..2 {
            let top = m.top();
            let calm = match top {
                P::Title => 0.0,
                P::Welcome => 0.4,
                _ => 1.0,
            };
            match frame {
                Some((_, shown)) => back.draw_world(&mut scr, ui.t, calm, shown),
                None => back.draw(&mut scr, ui.t, calm),
            }
            let under = scr.labels.len();
            ui.begin();
            let out = m.frame(&mut ui, &mut scr);
            ui.end();
            super::title::tidy(&mut scr, under);
            match out {
                Some(Go::Done(c)) => return c,
                Some(Go::To(p)) => {
                    m.stack.push(p);
                    ui.reset();
                    ui.focus = Some(0);
                }
                Some(Go::Back) => {
                    m.stack.pop();
                    if m.stack.is_empty() {
                        return Choice::Quit;
                    }
                    ui.reset();
                    ui.focus = Some(0);
                }
                None if !ui.acted => break,
                None => {}
            }
        }
        win.set_hand(ui.hot);
        match frame {
            Some((f, _)) => win.present(&scr, h, Some(f)),
            None => win.present(&scr, h, Some(back.frame((w as f32 * g.cell_w, h as f32 * g.cell_h), ui.t))),
        }
    }
}

enum Go {
    Done(Choice),
    To(P),
    Back,
}

/// A panel in the middle, `pw` wide, from row 3 to 3 from the bottom.
fn panel(s: &mut Screen, pw: f32) -> R {
    panel_h(s, pw, s.h as f32)
}

/// A panel in the middle, `pw` wide and at most `ph` tall.
fn panel_h(s: &mut Screen, pw: f32, ph: f32) -> R {
    let pw = pw.min(s.w as f32 - 4.0).max(20.0);
    let ph = ph.min(s.h as f32 - 5.0);
    let r = R::new((s.w as f32 - pw) * 0.5, ((s.h as f32 - ph) * 0.5).max(2.5), pw, ph);
    s.deco(Deco::panel(r.x, r.y, r.w, r.h, PANEL).border(PANEL_EDGE));
    r
}

fn title(s: &mut Screen, p: R, text: &str) {
    s.labels.push(left(p.x + 2.5, p.y + 2.1, text, 2.0, ACCENT, Face::Title));
}

/// Words wrapped in a column; returns the row after them.
fn para(s: &mut Screen, x: f32, mut y: f32, w: f32, text: &str, color: [u8; 3]) -> f32 {
    for l in crate::term::wrap(text, (w / 0.8).max(10.0) as usize) {
        s.labels.push(left(x, y + 0.5, &l, 0.95, color, Face::Body));
        y += 1.15;
    }
    y
}

impl Menus {
    fn top(&self) -> P {
        *self.stack.last().unwrap_or(&P::Title)
    }

    fn dropped(&mut self, p: &std::path::Path) {
        self.note = Some(match crate::share::take_in(p, &crate::log::dir().join("universes")) {
            Ok(added) => {
                self.list = picker::worlds();
                let at = added.canonicalize().ok();
                self.sel = self.list.iter().position(|w| w.path.canonicalize().ok() == at).unwrap_or(0);
                if !self.stack.contains(&P::Worlds) {
                    self.stack = vec![P::Title, P::Worlds];
                }
                format!("Added “{}” to your worlds.", self.list.get(self.sel).map_or("the world", |w| w.name.as_str()))
            }
            Err(e) => format!("Couldn't add {}: {e}", p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
        });
    }

    fn frame(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        match self.top() {
            P::Title => self.title(ui, s),
            P::Worlds => self.worlds(ui, s),
            P::New => self.new_world(ui, s),
            P::Together => self.together(ui, s),
            P::JoinCode => self.join_code(ui, s),
            P::Host => self.host(ui, s),
            P::Share => self.share(ui, s),
            P::Delete => self.delete(ui, s),
            P::Welcome => self.welcome(ui, s),
            P::Settings => {
                let pw = 90.0f32.min(s.w as f32 - 4.0);
                let area = R::new((s.w as f32 - pw) * 0.5, 2.0, pw, s.h as f32 - 4.0);
                let out = self.settings_page.draw(ui, s, area, &mut self.settings, &Live::default());
                out.done.then_some(Go::Back)
            }
        }
    }

    fn title(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        let (w, h) = (s.w as f32, s.h as f32);
        let cx = w * 0.5;
        let ty = (h * 0.2).max(3.5);
        // A soft shadow under the name and the line, so they read over any sky.
        s.title(cx + 0.12, ty + 0.1, "Infinite Pocket", 4.4, [20, 14, 8]);
        s.title(cx, ty, "Infinite Pocket", 4.4, ACCENT);
        s.labels.push(Label { x: cx + 0.08, y: ty + 3.06, text: "living worlds, written as code".into(), size: 1.1, color: [10, 10, 16], face: Face::Bold, center: true });
        s.labels.push(Label { x: cx, y: ty + 3.0, text: "living worlds, written as code".into(), size: 1.1, color: [240, 232, 250], face: Face::Bold, center: true });
        let bw = 32.0f32.min(w - 6.0);
        let x = cx - bw * 0.5;
        let mut y = (ty + 5.5).max(h * 0.38);
        // The world last played; failing that, the one most lately changed.
        let last = picker::last()
            .and_then(|p| {
                let p = p.canonicalize().unwrap_or(p);
                self.list.iter().find(|w| w.path.canonicalize().ok().as_ref() == Some(&p)).map(|w| (p, w.name.clone()))
            })
            .or_else(|| self.list.first().map(|w| (w.path.clone(), w.name.clone())));
        let mut go = None;
        let bh = 2.3;
        if let Some((p, name)) = &last {
            if ui.button(s, R::new(x, y, bw, bh), &format!("Continue · {name}"), Kind::Primary) {
                go = Some(Go::Done(Choice::Open(p.clone())));
            }
            y += bh + 0.8;
        }
        if self.list.is_empty() {
            // The first time: step into the world behind the menus.
            if ui.button(s, R::new(x, y, bw, bh), &format!("Open {}", crate::starter::NAME), Kind::Primary) {
                go = self.starter().map(|p| Go::Done(Choice::Open(p)));
            }
            y += bh + 0.8;
            if ui.button(s, R::new(x, y, bw, bh), "Make a new world", Kind::Normal) {
                self.draft = Draft::rolled();
                go = Some(Go::To(P::New));
            }
        } else {
            let two = R::new(x, y, bw, bh).cols(2, 0.8);
            if ui.button(s, two[0], "Your worlds", Kind::Normal) {
                go = Some(Go::To(P::Worlds));
            }
            if ui.button(s, two[1], "New world", Kind::Normal) {
                self.draft = Draft::rolled();
                go = Some(Go::To(P::New));
            }
        }
        y += bh + 0.8;
        if ui.button(s, R::new(x, y, bw, bh), "Play together", Kind::Normal) {
            go = Some(Go::To(P::Together));
        }
        y += bh + 0.8;
        let half = R::new(x, y, bw, bh).cols(2, 0.8);
        if ui.button(s, half[0], "Settings", Kind::Normal) {
            go = Some(Go::To(P::Settings));
        }
        if ui.button(s, half[1], "Quit", Kind::Normal) || ui.back() {
            go = Some(Go::Done(Choice::Quit));
        }
        y += bh + 1.2;
        if !self.settings.has_llm() {
            note(s, cx, y + 0.5, "Add an AI key in Settings → AI to bring worlds to life.", [255, 214, 150]);
        }
        if let Some(n) = &self.note {
            note(s, cx, y + 1.8, n, ACCENT);
        }
        s.labels.push(left(1.5, h - 1.0, &crate::db::build_name(), 0.8, FAINT, Face::Body));
        if let Some(n) = crate::net::update::newer() {
            let how = if crate::net::update::from_brew() { "brew upgrade pocket" } else { n.url.trim_start_matches("https://") };
            let t = format!("{} is out: {how}", n.version);
            s.labels.push(Label { x: w - 1.5 - t.chars().count() as f32 * 0.8, y: h - 1.5, text: t, size: 0.8, color: ACCENT, face: Face::Body, center: false });
        }
        go
    }

    /// A new Maple Cove of your own (or why not).
    fn starter(&mut self) -> Option<PathBuf> {
        match crate::starter::new_copy() {
            Ok(p) => Some(p),
            Err(e) => {
                crate::log::error(format!("copy Maple Cove: {e:#}"));
                self.note = Some(format!("Couldn't open Maple Cove: {e}"));
                None
            }
        }
    }

    fn worlds(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        let p = panel(s, 96.0);
        title(s, p, "Worlds");
        let cw = p.w - 4.0;
        let x = p.x + 2.0;
        let top = p.y + 4.2;
        let bottom = p.y + p.h - 6.2;
        let ch = 4.0;
        let gap = 0.6;
        let fits = (((bottom - top) / (ch + gap)).floor() as usize).max(1);
        self.sel = self.sel.min(self.list.len().saturating_sub(1));
        if ui.wheel != 0.0 {
            self.first = (self.first as i32 - ui.wheel.signum() as i32).max(0) as usize;
        }
        self.first = self.first.min(self.sel).max((self.sel + 1).saturating_sub(fits)).min(self.list.len().saturating_sub(fits));
        let mut go = None;
        if self.list.is_empty() {
            note(s, p.x + p.w * 0.5, top + 3.0, "No worlds yet.", DIM);
        }
        let area = R::new(x, top, cw, fits as f32 * (ch + gap));
        let (enter, glow) = ui.list(area, self.list.len(), &mut self.sel);
        if enter && !self.list.is_empty() {
            go = Some(Go::Done(Choice::Open(self.list[self.sel].path.clone())));
        }
        self.first = self.first.min(self.sel).max((self.sel + 1).saturating_sub(fits));
        for i in self.first..(self.first + fits).min(self.list.len()) {
            let y = top + (i - self.first) as f32 * (ch + gap);
            let r = R::new(x, y, cw, ch);
            let it = ui.card(s, r, i == self.sel, glow);
            if it.click {
                self.sel = i;
            }
            if it.double {
                go = Some(Go::Done(Choice::Open(self.list[i].path.clone())));
            }
            let wd = &self.list[i];
            let when = picker::ago(wd.modified);
            s.labels.push(left(x + 1.5, y + 1.05, &fit(&wd.name, cw - 16.0, 1.3), 1.3, if i == self.sel { ACCENT } else { TEXT }, Face::Title));
            s.labels.push(Label { x: x + cw - 1.5 - when.chars().count() as f32 * 0.8, y: y + 0.55, text: when, size: 0.85, color: DIM, face: Face::Body, center: false });
            s.labels.push(left(x + 1.5, y + 2.25, &fit(&format!("“{}”", wd.prompt), cw - 4.0, 0.9), 0.9, DIM, Face::Body));
            let stats = format!("{} · {} · {}", plural(wd.regions, "place", "places"), plural(wd.people, "person", "people"), plural(wd.creatures, "creature", "creatures"));
            s.labels.push(left(x + 1.5, y + 3.25, &stats, 0.8, FAINT, Face::Body));
        }
        if self.list.len() > fits {
            note(s, p.x + p.w * 0.5, bottom + 0.3, &format!("{}–{} of {}", self.first + 1, (self.first + fits).min(self.list.len()), self.list.len()), FAINT);
        }
        // Buttons: two rows, as in many a game.
        let any = !self.list.is_empty();
        let row1 = R::new(x, p.y + p.h - 5.4, cw, 2.2).cols(2, 0.8);
        if ui.button_on(s, row1[0], "Play", Kind::Primary, any) {
            go = Some(Go::Done(Choice::Open(self.list[self.sel].path.clone())));
        }
        if ui.button(s, row1[1], "New world", Kind::Normal) || ui.letter('n') {
            self.draft = Draft::rolled();
            go = Some(Go::To(P::New));
        }
        let row2 = R::new(x, p.y + p.h - 2.8, cw, 2.0).cols(3, 0.8);
        if ui.button_on(s, row2[0], "Share", Kind::Normal, any) {
            self.sharing = Sharing::Asking;
            go = Some(Go::To(P::Share));
        }
        if ui.button_on(s, row2[1], "Delete", Kind::Danger, any) || (any && ui.take(crossterm::event::KeyCode::Delete)) {
            go = Some(Go::To(P::Delete));
        }
        if ui.button(s, row2[2], "Back", Kind::Quiet) || ui.back() {
            go = Some(Go::Back);
        }
        let line = self.note.clone().unwrap_or_else(|| "Drop a .pocket file on this window to add a world someone shared.".into());
        note(s, p.x + p.w * 0.5, p.y + 3.6, &fit(&line, p.w - 30.0, 0.92), if self.note.is_some() { ACCENT } else { FAINT });
        if go.is_some() {
            self.note = None;
        }
        go
    }

    fn new_world(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        let p = panel(s, 90.0);
        title(s, p, "New world");
        let x = p.x + 2.0;
        let cw = p.w - 4.0;
        let mut go = None;
        let mut y = p.y + 4.4;
        s.labels.push(left(x + 0.5, y, "What should it be? A place, a time, a story…", 0.95, DIM, Face::Body));
        y += 1.2;
        // The words, in a box that grows to five lines.
        let tw = (cw - 4.0).max(8.0) as usize;
        let lines = self.draft.lines(tw);
        let shown = lines.len().clamp(2, 5);
        let bx = R::new(x, y, cw, shown as f32 + 1.4);
        let (_, focused, keys, pasted) = ui.area(bx);
        for t in pasted {
            self.draft.insert(&t.replace(['\n', '\r'], " "));
        }
        for k in keys {
            match self.draft.key(k) {
                Edit::Done(prompt) if self.settings.has_llm() => go = Some(Go::Done(Choice::New(prompt, self.difficulty))),
                Edit::Done(_) => {}
                Edit::Cancel => go = Some(Go::Back),
                Edit::Stay => {}
            }
        }
        let lines = self.draft.lines(tw);
        s.deco(Deco::panel(bx.x, bx.y, bx.w, bx.h, [18, 20, 28]).border(if focused { ACCENT } else { PANEL_EDGE }));
        let at = lines.iter().rposition(|(a, _)| *a <= self.draft.cursor).unwrap_or(0);
        let first = at.saturating_sub(shown - 1);
        let blink = (ui.t * 2.0) as i64 % 2 == 0;
        for (row, (a, b)) in lines.iter().enumerate().skip(first).take(shown) {
            let mut t: String = self.draft.text[*a..*b].iter().collect();
            if focused && !self.draft.fresh && blink && self.draft.cursor >= *a && (self.draft.cursor < *b || (row == lines.len() - 1 && self.draft.cursor == *b)) {
                let i = self.draft.cursor - a;
                t = format!("{}▏{}", t.chars().take(i).collect::<String>(), t.chars().skip(i).collect::<String>());
            }
            let color = if self.draft.fresh { [240, 214, 170] } else { TEXT };
            s.labels.push(left(bx.x + 1.5, bx.y + 1.2 + (row - first) as f32, &t, 1.0, color, Face::Body));
        }
        if self.draft.text.is_empty() && focused && blink {
            s.labels.push(left(bx.x + 1.5, bx.y + 1.2, "▏", 1.0, ACCENT, Face::Body));
        }
        y = bx.y + bx.h + 0.6;
        let b = R::new(x, y, cw, 1.8).cols(3, 0.8);
        if ui.button(s, b[0], "Another random place", Kind::Normal) {
            self.draft = Draft::rolled();
        }
        if ui.button(s, b[1], "Clear, write my own", Kind::Normal) {
            self.draft.clear();
            ui.focus = Some(0);
        }
        if self.draft.fresh {
            s.labels.push(left(b[2].x + 0.5, b[2].mid_y(), "a place picked at random", 0.85, FAINT, Face::Body));
        }
        y += 2.6;
        let mut rows = Rows { x, y, w: cw, top: y, bottom: p.y + p.h - 3.4 };
        ui.clip = Some((rows.top, rows.bottom));
        let levels = crate::sim::night::LEVELS;
        let r = rows.next(ROW);
        let d = ui.choice(s, r, "Difficulty", &cap(levels[self.difficulty].name), false);
        if d != 0 {
            self.difficulty = (self.difficulty as i32 + d).clamp(0, levels.len() as i32 - 1) as usize;
        }
        rows.note(s, difficulty_words(self.difficulty));
        // The models are chosen in Settings → AI; here only the key, if none yet.
        let changed = if self.settings.has_llm() {
            rows.note(s, "Making a world costs about $4, and playing about $1 an hour.");
            false
        } else if self.settings.provider != "other" {
            rows.head(s, "Claude API key");
            let r = rows.next(ROW);
            let k = ui.field(s, r, "Key", &mut self.settings.api_key, "sk-ant-…", true, false);
            let r = rows.next(ROW);
            if ui.button(s, R::new(r.x, r.y, 34.0f32.min(r.w), r.h), "Get a key at platform.claude.com", Kind::Normal) {
                if let Err(e) = crate::share::open_url(KEYS_URL) {
                    crate::log::error(format!("open the browser: {e:#}"));
                }
            }
            rows.note(s, "A world is made by Claude, so it needs a key. Making one costs about $4, and playing about $1 an hour.");
            k
        } else {
            rows.head(s, "AI");
            let c = page::ai(ui, s, &mut rows, &mut self.settings);
            if !self.settings.has_llm() {
                rows.note(s, "A world is made by the AI, so it needs one set up.");
            }
            c
        };
        if changed {
            self.settings.save();
        }
        ui.clip = None;
        let b = R::new(p.x + p.w - 38.0, p.y + p.h - 2.9, 36.0, 2.1).cols(2, 0.8);
        let prompt: String = self.draft.text.iter().collect();
        if ui.button_on(s, b[1], "Create world", Kind::Primary, !prompt.trim().is_empty() && self.settings.has_llm()) {
            go = Some(Go::Done(Choice::New(prompt.trim().to_string(), self.difficulty)));
        }
        if ui.button(s, b[0], "Back", Kind::Quiet) || ui.back() {
            go = Some(Go::Back);
        }
        go
    }

    fn together(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        let p = panel(s, 84.0);
        title(s, p, "Play together");
        let x = p.x + 2.0;
        let cw = p.w - 4.0;
        let mut go = None;
        let mut y = p.y + 4.6;
        s.labels.push(left(x + 0.5, y, "Games on your network", 1.0, [150, 170, 220], Face::Title));
        y += 1.6;
        if self.found.is_empty() {
            // Some networks keep the host's call from reaching anyone.
            let long = self.listen.as_ref().is_none_or(|l| l.listening_for() > std::time::Duration::from_secs(6));
            let say = if long { "None found. Some networks keep games from seeing each other: choose Join with an invite and type the address the host was shown, and the code." } else { "None yet. When someone on this Wi-Fi hosts a world, it shows here by itself." };
            y = para(s, x + 0.5, y, cw - 1.0, say, DIM) + 0.6;
        }
        if self.game.is_some_and(|g| g >= self.found.len()) {
            self.game = None;
        }
        let shown = self.found.len().min(4);
        let mut pick = self.game.unwrap_or(0);
        let (enter, glow) = ui.list(R::new(x, y, cw, shown as f32 * 3.0), shown, &mut pick);
        if glow && shown > 0 {
            self.game = Some(pick);
        }
        if enter && shown > 0 {
            self.addr = self.found[pick].addr.clone();
            go = Some(Go::To(P::JoinCode));
        }
        for (i, g) in self.found.iter().enumerate().take(4) {
            let r = R::new(x, y, cw, 2.6);
            let it = ui.card(s, r, self.game == Some(i), glow);
            if it.click {
                self.game = Some(i);
            }
            if it.double {
                self.addr = g.addr.clone();
                go = Some(Go::To(P::JoinCode));
            }
            s.labels.push(left(x + 1.5, r.mid_y(), &fit(&format!("{}'s “{}”", g.host, g.world), cw - 30.0, 1.0), 1.0, TEXT, Face::Bold));
            let side = if g.same_build { g.addr.clone() } else { format!("another version ({})", g.build) };
            s.labels.push(Label { x: x + cw - 1.5 - side.chars().count() as f32 * 0.8, y: r.mid_y() - 0.45, text: side, size: 0.85, color: DIM, face: Face::Body, center: false });
            y += 3.0;
        }
        let row = R::new(x, p.y + p.h - 5.4, cw, 2.2).cols(2, 0.8);
        if ui.button_on(s, row[0], "Join", Kind::Primary, self.game.is_some()) {
            if let Some(g) = self.game.and_then(|g| self.found.get(g)) {
                self.addr = g.addr.clone();
            }
            go = Some(Go::To(P::JoinCode));
        }
        if ui.button(s, row[1], "Join with an invite…", Kind::Normal) {
            go = Some(Go::To(P::JoinCode));
        }
        let row = R::new(x, p.y + p.h - 2.8, cw, 2.0).cols(2, 0.8);
        if ui.button_on(s, row[0], "Host one of my worlds…", Kind::Normal, !self.list.is_empty()) {
            self.host = self.sel;
            go = Some(Go::To(P::Host));
        }
        if ui.button(s, row[1], "Back", Kind::Quiet) || ui.back() {
            go = Some(Go::Back);
        }
        go
    }

    fn join_code(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        let p = panel_h(s, 76.0, 22.0);
        title(s, p, "Join a world");
        let x = p.x + 2.0;
        let cw = p.w - 4.0;
        let mut y = p.y + 4.4;
        y = para(s, x + 0.5, y, cw - 1.0, "Paste the invite the host sent you, or type their address and code.", DIM) + 0.8;
        let mut go = None;
        let paste = R::new(x, y, 24.0f32.min(cw), 2.0);
        if ui.button(s, paste, "Paste invite", Kind::Normal) {
            if let Some(t) = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()) {
                self.take_invite(&t);
            }
        }
        y += 3.0;
        // An invite pasted into either field fills both.
        let invite = ui.pasted.iter().find(|t| t.split_whitespace().count() == 2).cloned();
        if let Some(t) = invite {
            ui.pasted.clear();
            self.take_invite(&t);
        }
        let mut rows = Rows { x, y, w: cw, top: y, bottom: p.y + p.h };
        let r = rows.next(ROW);
        ui.field(s, r, "Address", &mut self.addr, "192.168.1.20:7777 or https://….ngrok-free.app", false, false);
        let r = rows.next(ROW);
        ui.field(s, r, "Code", &mut self.code, "the host's code", false, false);
        if let Some(n) = &self.note {
            note(s, p.x + p.w * 0.5, rows.y + 1.0, n, ACCENT);
        }
        let b = R::new(p.x + p.w - 38.0, p.y + p.h - 2.9, 36.0, 2.1).cols(2, 0.8);
        let ready = !self.addr.trim().is_empty() && !self.code.trim().is_empty();
        if ui.button_on(s, b[1], "Join", Kind::Primary, ready) || (ready && ui.take(crossterm::event::KeyCode::Enter)) {
            go = Some(Go::Done(Choice::Join(self.addr.trim().to_string(), self.code.trim().to_string())));
        }
        if ui.button(s, b[0], "Back", Kind::Quiet) || ui.back() {
            go = Some(Go::Back);
        }
        go
    }

    fn take_invite(&mut self, t: &str) {
        let parts: Vec<&str> = t.split_whitespace().collect();
        if let [a, c] = parts.as_slice() {
            self.addr = a.to_string();
            self.code = c.to_string();
        } else if let Some(a) = parts.first() {
            self.addr = a.to_string();
        }
    }

    fn host(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        let p = panel_h(s, 80.0, 22.0);
        title(s, p, "Host a world");
        let x = p.x + 2.0;
        let cw = p.w - 4.0;
        let mut go = None;
        let y = p.y + 4.6;
        let mut rows = Rows { x, y, w: cw, top: y, bottom: p.y + p.h };
        // The most recent world first; with none yet, Maple Cove (made when hosted).
        let mut names: Vec<String> = self.list.iter().map(|w| w.name.clone()).collect();
        if names.is_empty() {
            names.push(crate::starter::NAME.into());
        }
        self.host = self.host.min(names.len().saturating_sub(1));
        let r = rows.next(ROW);
        if let Some(i) = ui.dropdown(s, r, "World", &names, self.host, false) {
            self.host = i;
        }
        rows.y += 0.6;
        let text = "Friends join and play in it with you. It runs on this computer and uses your AI for what they say and make (up to what you allow each guest in Settings → Together). They need no key and no copy.";
        rows.y = para(s, x + 1.0, rows.y, cw - 2.0, text, DIM) + 0.6;
        if !self.ngrok {
            para(s, x + 1.0, rows.y, cw - 2.0, "To host for friends outside this network, install ngrok (ngrok.com/download) and sign in once.", FAINT);
        }
        let path = self.list.get(self.host).map(|w| w.path.clone());
        let b = R::new(x, p.y + p.h - 5.4, cw, 2.2).cols(2, 0.8);
        let mut internet = None;
        if ui.button(s, b[0], "Host on this network", Kind::Primary) {
            internet = Some(false);
        }
        if ui.button_on(s, b[1], "Host for anyone (ngrok)", Kind::Normal, self.ngrok) {
            internet = Some(true);
        }
        if let Some(i) = internet {
            go = path.or_else(|| self.starter()).map(|p| Go::Done(Choice::Host(p, i)));
        }
        let back = R::new(p.x + p.w - 20.0, p.y + p.h - 2.8, 18.0, 2.0);
        if ui.button(s, back, "Back", Kind::Quiet) || ui.back() {
            go = Some(Go::Back);
        }
        go
    }

    fn share(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        let p = panel_h(s, 72.0, 20.0);
        title(s, p, "Share a world");
        let Some(wd) = self.list.get(self.sel) else { return Some(Go::Back) };
        let x = p.x + 2.5;
        let cw = p.w - 5.0;
        let mut y = p.y + 4.6;
        s.labels.push(left(x, y, &fit(&wd.name, cw, 1.3), 1.3, TEXT, Face::Title));
        y += 2.2;
        let mut go = None;
        let b = R::new(x, p.y + p.h - 2.9, cw, 2.1).cols(2, 0.8);
        match &self.sharing {
            Sharing::Asking => {
                y = para(s, x, y, cw, "Makes one file of this world to send to anyone: the land, everything made in it, its people and what they remember.", TEXT) + 0.6;
                para(s, x, y, cw, "It leaves you out: your health, what you carry, how you look, where you stand. No keys are ever in it.", DIM);
                if ui.button(s, b[1], "Make the file", Kind::Primary) {
                    self.sharing = match crate::share::make_copy(&wd.path, &crate::share::copies_dir()) {
                        Ok(p) => Sharing::Made(p),
                        Err(e) => {
                            crate::log::error(format!("share {}: {e:#}", wd.path.display()));
                            Sharing::Failed(format!("{e:#}"))
                        }
                    };
                }
            }
            Sharing::Made(f) => {
                let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                y = para(s, x, y, cw, &format!("Saved {name} ({}) in {}.", picker::size_text(f), f.parent().map(|d| d.display().to_string()).unwrap_or_default()), TEXT) + 0.6;
                para(s, x, y, cw, "Send it any way you like. To play it, they drop it on the Infinite Pocket window.", DIM);
                if ui.button(s, b[1], crate::share::browser_name(), Kind::Primary) {
                    if let Err(e) = crate::share::reveal(f) {
                        crate::log::error(format!("reveal {}: {e:#}", f.display()));
                    }
                }
            }
            Sharing::Failed(e) => {
                para(s, x, y, cw, &format!("The file couldn't be made: {e}"), DIM);
            }
        }
        if ui.button(s, b[0], "Back", Kind::Quiet) || ui.back() {
            go = Some(Go::Back);
        }
        go
    }

    fn welcome(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        if let Some(r) = self.checking.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.checking = None;
            let ok = r.is_ok();
            self.checked = Some(r);
            if ok {
                self.settings.welcomed = true;
                self.settings.save();
            }
        }
        let pw = 72.0f32.min(s.w as f32 - 4.0);
        let ph = 23.0f32.min(s.h as f32 - 2.0);
        let r = R::new((s.w as f32 - pw) * 0.5, ((s.h as f32 - ph) * 0.5).max(1.0), pw, ph);
        s.deco(Deco::panel(r.x, r.y, r.w, r.h, PANEL).border(PANEL_EDGE));
        let x = r.x + 2.5;
        let cw = r.w - 5.0;
        s.labels.push(left(x, r.y + 2.3, "Welcome to Infinite Pocket", 2.0, ACCENT, Face::Title));
        let mut y = r.y + 4.4;
        y = para(s, x, y, cw, "Every world here is made from a few words you write: its land, its people, what they say and make. An AI writes it all as code.", TEXT) + 0.5;
        y = para(s, x, y, cw, "To begin, add a Claude API key. Making a new world costs about $4, and playing about $1 an hour. The key stays on this computer, and you can set a spending limit in Settings.", DIM) + 0.8;
        let worked = matches!(self.checked, Some(Ok(())));
        let mut go = None;
        // The key, and a button to paste it.
        let paste = R::new(r.x + r.w - 14.5, y, 12.0, 2.0);
        let field = R::new(r.x + 1.0, y, r.w - 16.5, 2.0);
        let before = self.settings.api_key.clone();
        ui.field(s, field, "", &mut self.settings.api_key, "Paste your key here (sk-ant-…)", true, false);
        if ui.button(s, paste, "Paste", Kind::Normal) {
            if let Some(t) = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()) {
                self.settings.api_key = t.trim().to_string();
            }
        }
        if self.settings.api_key != before {
            self.checked = None;
        }
        y += 2.6;
        let (line, color) = match (&self.checking, &self.checked) {
            (Some(_), _) => ("Checking the key…".to_string(), DIM),
            (_, Some(Ok(()))) => ("✓ The key works. You're ready.".to_string(), [150, 220, 150]),
            (_, Some(Err(e))) => (e.clone(), [240, 150, 130]),
            _ => (String::new(), DIM),
        };
        s.labels.push(left(x, y + 0.5, &line, 0.95, color, Face::Body));
        let row = R::new(r.x + 2.0, r.y + r.h - 5.6, r.w - 4.0, 2.1).cols(2, 0.8);
        if ui.button(s, row[0], "Get a key at platform.claude.com", Kind::Normal) {
            if let Err(e) = crate::share::open_url(KEYS_URL) {
                crate::log::error(format!("open the browser: {e:#}"));
            }
        }
        if ui.button(s, row[1], "Use another AI…", Kind::Normal) {
            self.settings.welcomed = true;
            self.settings.save();
            self.settings_page.tab = page::TABS.iter().position(|t| *t == "AI").unwrap_or(0);
            self.stack.pop();
            go = Some(Go::To(P::Settings));
        }
        let row = R::new(r.x + 2.0, r.y + r.h - 3.0, r.w - 4.0, 2.1).cols(2, 0.8);
        if ui.button(s, row[0], "Just look around (no one talks)", Kind::Quiet) || ui.back() {
            self.settings.welcomed = true;
            self.settings.save();
            go = Some(Go::Back);
        }
        let label = if worked { "Let's go" } else { "Check and save" };
        let ready = !self.settings.api_key.trim().is_empty() && self.checking.is_none();
        if ui.button_on(s, row[1], label, Kind::Primary, ready) || (ready && ui.take(crossterm::event::KeyCode::Enter)) {
            if worked {
                go = Some(Go::Back);
            } else {
                // Saved now; checked in the background.
                self.settings.provider = "anthropic".into();
                self.settings.save();
                let key = self.settings.api_key.clone();
                let (tx, rx) = crossbeam_channel::bounded(1);
                std::thread::spawn(move || {
                    let _ = tx.send(crate::llm::check_key(&key));
                });
                self.checking = Some(rx);
            }
        }
        go
    }

    fn delete(&mut self, ui: &mut Ui, s: &mut Screen) -> Option<Go> {
        let Some(wd) = self.list.get(self.sel) else { return Some(Go::Back) };
        let pw = 60.0f32.min(s.w as f32 - 4.0);
        let r = R::new((s.w as f32 - pw) * 0.5, s.h as f32 * 0.5 - 5.0, pw, 10.0);
        s.deco(Deco::panel(r.x, r.y, r.w, r.h, PANEL).border(PANEL_EDGE));
        s.labels.push(left(r.x + 2.5, r.y + 2.0, "Delete this world?", 1.6, ACCENT, Face::Title));
        para(s, r.x + 2.5, r.y + 3.6, r.w - 5.0, &format!("“{}” will be gone for good.", wd.name), TEXT);
        let b = R::new(r.x + 2.0, r.y + r.h - 3.0, r.w - 4.0, 2.1).cols(2, 0.8);
        if ui.button(s, b[0], "Keep it", Kind::Normal) || ui.back() {
            return Some(Go::Back);
        }
        if ui.button(s, b[1], "Delete", Kind::Danger) {
            if let Err(e) = picker::delete_world(&wd.path) {
                crate::log::error(format!("delete {}: {e}", wd.path.display()));
            }
            self.list = picker::worlds();
            self.sel = self.sel.min(self.list.len().saturating_sub(1));
            return Some(Go::Back);
        }
        None
    }
}

/// Where Claude API keys are made.
pub const KEYS_URL: &str = "https://platform.claude.com/settings/keys";

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn difficulty_words(i: usize) -> &'static str {
    match i {
        0 => "Nights are calm. Nothing comes for you.",
        1 => "Some nights, something stirs. It is slow to find you.",
        2 => "Nights bring things that hunt. Find shelter, or light.",
        _ => "Nights are long and the dark is hungry.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menus_for(list: Vec<World>) -> Menus {
        Menus {
            stack: vec![P::Title],
            list,
            sel: 0,
            first: 0,
            note: None,
            settings: Settings::default(),
            settings_page: page::Page::default(),
            draft: Draft::rolled(),
            difficulty: NEW_DIFFICULTY,
            sharing: Sharing::Asking,
            listen: None,
            found: Vec::new(),
            game: None,
            addr: String::new(),
            code: String::new(),
            host: 0,
            ngrok: false,
            checking: None,
            checked: None,
        }
    }

    fn world(name: &str) -> World {
        World { path: format!("/w/{name}.pocket").into(), name: name.into(), prompt: "a quiet farm".into(), regions: 3, people: 2, creatures: 1, modified: std::time::SystemTime::UNIX_EPOCH }
    }

    /// Draw every page at a few sizes; each says what it is.
    #[test]
    fn every_page_draws_at_any_size() {
        let back = Backdrop::new();
        for (w, h) in [(60, 24), (130, 40), (200, 60)] {
            for list in [vec![], vec![world("Salt Flats"), world("Harrowcove")]] {
                let mut m = menus_for(list);
                for p in [P::Title, P::Worlds, P::New, P::Together, P::JoinCode, P::Host, P::Share, P::Delete, P::Settings, P::Welcome] {
                    m.stack = vec![P::Title, p];
                    let mut s = Screen::new(w, h, true);
                    let mut ui = Ui::new();
                    back.draw(&mut s, 1.0, 1.0);
                    ui.begin();
                    m.frame(&mut ui, &mut s);
                    ui.end();
                    let text = s.plain_text();
                    let want = match p {
                        P::Title => "Infinite Pocket",
                        P::Worlds => "Worlds",
                        P::New => "New world",
                        P::Together => "Play together",
                        P::JoinCode => "Join a world",
                        P::Host => "Host a world",
                        P::Share | P::Delete if m.list.is_empty() => continue,
                        P::Share => "Share a world",
                        P::Delete => "Delete this world?",
                        P::Settings => "Settings",
                        P::Welcome => "Welcome to Infinite Pocket",
                    };
                    assert!(text.contains(want), "{p:?} at {w}x{h}: {text}");
                }
            }
        }
    }

    #[test]
    fn enter_on_new_world_creates_it() {
        let mut m = menus_for(vec![]);
        m.stack = vec![P::Title, P::New];
        m.draft = Draft::given("a quiet farm");
        let mut s = Screen::new(130, 40, true);
        let mut ui = Ui::new();
        ui.focus = Some(0);
        ui.key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE));
        ui.begin();
        let go = m.frame(&mut ui, &mut s);
        ui.end();
        assert!(matches!(go, Some(Go::Done(Choice::New(p, 2))) if p == "a quiet farm"));
    }
}
