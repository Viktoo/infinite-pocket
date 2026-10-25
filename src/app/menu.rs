//! The pause menu (Esc): back to the game, achievements, creations, this
//! world's options, the settings (see `ui::settings`), and the ways out.
//! All buttons; the keys work them too.

use super::{ACCENT, App, DIM, TEXT};
use crate::ui::{Kind, R};
use crate::achievements::{self, Tier};
use crossterm::event::{KeyCode, KeyEvent};

pub(super) const HEAD: [u8; 3] = [150, 170, 220];

const CREATURES: &[f64] = &[6.0, 12.0, 24.0, 48.0, 96.0, 200.0];
const LIFE: &[f64] = &[0.25, 0.5, 1.0, 2.0, 4.0, 8.0];

/// Which page of the pause menu is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Pause,
    Settings,
    World,
    Details,
    Achievements,
    Creations,
    /// Opening this world to friends: on this network, or for anyone.
    Open,
    /// Hosting: the invite to give, who is here.
    Invite,
}

pub struct Menu {
    pub page: Page,
    /// The spend details page: this world's total and the latest calls.
    details: Option<(f64, Vec<(f64, String, f64)>)>,
    /// The chosen achievement on that page, and how far it is scrolled.
    ach_sel: usize,
    pub(super) ach_scroll: f32,
    /// The creations page, when open.
    pub creations: Option<super::creations::Book>,
    settings: crate::ui::settings::Page,
    /// The page just changed (the controls start over).
    fresh: bool,
    /// Why hosting couldn't start.
    open_err: Option<String>,
    /// The invite's address shown, not dots (it can be read off a recording).
    show_addr: bool,
}

/// A medal: more ornate as the challenge grows.
pub fn icon(t: Tier) -> &'static str {
    match t {
        Tier::Bronze => "(•)",
        Tier::Silver => "(✦)",
        Tier::Gold => "«★»",
        Tier::Diamond => "✧◆✧",
    }
}

/// A medal drawn at `x`, `y`, `h` rows tall (twice as many cells wide, so
/// round): a ribbon behind, a rim, a face and the tier's mark on it (one,
/// two, three stars; a diamond). Not yet earned: grey, a question mark, the
/// tier's colour faintly round its rim. Its width.
pub fn medal(s: &mut crate::term::Screen, x: f32, y: f32, h: f32, t: Tier, earned: bool) -> f32 {
    use crate::term::{Deco, Face, Label};
    let w = h * 2.0;
    let cx = x + w * 0.5;
    let c = tier_color(t);
    let shade = |c: [u8; 3], k: f32| c.map(|v| (v as f32 * k).min(255.0) as u8);
    let disc = |s: &mut crate::term::Screen, inset: f32, fill: [u8; 3], border: Option<[u8; 3]>| {
        let (hh, ww) = (h - 2.0 * inset, w - 4.0 * inset);
        s.deco(Deco { x: cx - ww * 0.5, y: y + inset, w: ww, h: hh, fill, alpha: 1.0, radius: hh * 0.5, shadow: false, border });
    };
    if !earned {
        disc(s, 0.0, [40, 43, 60], Some(shade(c, 0.45)));
        disc(s, h * 0.14, [30, 32, 46], None);
        s.labels.push(Label { x: cx, y: y + h * 0.5, text: "?".into(), size: h * 0.42, color: [92, 96, 120], face: Face::Bold, center: true });
        return w;
    }
    // The ribbon's two tails, under the medal.
    let ribbon = match t {
        Tier::Bronze => [170, 62, 52],
        Tier::Silver => [64, 104, 176],
        Tier::Gold => [176, 36, 64],
        Tier::Diamond => [104, 70, 190],
    };
    for (dx, k) in [(-0.62, 1.0), (0.12, 0.8)] {
        s.deco(Deco { x: cx + dx * h, y: y + h * 0.55, w: h * 0.5, h: h * 0.62, fill: shade(ribbon, k), alpha: 1.0, radius: 0.08, shadow: false, border: None });
    }
    disc(s, 0.0, shade(c, 0.62), Some(shade(c, 1.25)));
    disc(s, h * 0.13, c, None);
    // A soft light on its upper half.
    s.deco(Deco { x: cx - w * 0.28, y: y + h * 0.2, w: w * 0.56, h: h * 0.3, fill: [255, 255, 255], alpha: 0.1, radius: h * 0.15, shadow: false, border: None });
    let (mark, size) = match t {
        Tier::Bronze => ("★", 0.62),
        Tier::Silver => ("★★", 0.5),
        Tier::Gold => ("★★★", 0.42),
        Tier::Diamond => ("◆", 0.62),
    };
    s.labels.push(Label { x: cx, y: y + h * 0.52, text: mark.into(), size: h * size, color: shade(c, 0.32), face: Face::Bold, center: true });
    w
}

pub fn tier_color(t: Tier) -> [u8; 3] {
    match t {
        Tier::Bronze => [205, 127, 50],
        Tier::Silver => [200, 208, 222],
        Tier::Gold => [255, 200, 70],
        Tier::Diamond => [140, 225, 255],
    }
}

pub fn tier_name(t: Tier) -> &'static str {
    match t {
        Tier::Bronze => "bronze",
        Tier::Silver => "silver",
        Tier::Gold => "gold",
        Tier::Diamond => "diamond",
    }
}

/// What a paid call was for, in plain words.
fn purpose(p: &str) -> &str {
    match p {
        "dialogue" | "chat" => "conversation",
        "decide" => "someone deciding what to do",
        "create" => "building something",
        "type" => "a new kind of thing",
        "interpret" => "an action",
        "region" => "a new region",
        "genesis" => "the world",
        "summary" => "a memory",
        "gesture" => "a new gesture",
        other => other,
    }
}

pub(super) fn ago(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86400),
    }
}

impl App {
    pub(super) fn toggle_menu(&mut self) {
        self.open_menu(false);
    }

    /// Open the pause menu (or its achievements page), or close it.
    pub(super) fn open_menu(&mut self, achievements: bool) {
        self.menu = match self.menu {
            Some(_) => None,
            None => Some(Menu { page: if achievements { Page::Achievements } else { Page::Pause }, details: None, ach_sel: 0, ach_scroll: 0.0, creations: None, settings: Default::default(), fresh: false, open_err: None, show_addr: false }),
        };
        if self.log_view == 2 {
            self.set_log_view(0);
        }
        self.keys.down.clear();
        self.ui.reset();
        self.ui.focus = Some(first_focus(if achievements { Page::Achievements } else { Page::Pause }));
        self.dirty = true;
    }

    /// The settings' AI tab, where a key goes.
    pub(super) fn open_ai_settings(&mut self) {
        if self.menu.is_none() {
            self.open_menu(false);
        }
        self.menu_page(Page::Settings);
        if let Some(m) = self.menu.as_mut() {
            m.settings.tab = crate::ui::settings::TABS.iter().position(|t| *t == "AI").unwrap_or(0);
        }
    }

    pub(super) fn menu_page(&mut self, p: Page) {
        if p == Page::Creations {
            self.open_creations();
        }
        if let Some(m) = self.menu.as_mut() {
            m.page = p;
            m.fresh = true;
        }
        self.ui.reset();
        self.ui.focus = Some(first_focus(p));
        self.dirty = true;
    }

    /// The pointer moved (in cells): menus follow it.
    pub fn pointer(&mut self, x: f32, y: f32) {
        self.ui.feed(&crate::window::Ev::Mouse(x, y));
        if self.menu.is_some() {
            self.dirty = true;
        }
    }

    /// The pointer is over a button of the menu (show a hand).
    pub fn pointer_hand(&self) -> bool {
        self.menu.is_some() && self.ui.hot
    }

    pub fn click(&mut self, x: f32, y: f32) {
        self.ui.press_at(x, y);
        self.dirty = true;
    }

    pub fn scroll(&mut self, d: f32) {
        if self.menu.is_some() {
            self.ui.feed(&crate::window::Ev::Scroll(d));
            self.dirty = true;
        } else if self.log_view > 0 {
            // The journal scrolls back with the wheel; two fingers on a
            // trackpad send small steps, added up to whole lines.
            self.wheel_rows += d;
            let rows = self.wheel_rows.trunc();
            self.wheel_rows -= rows;
            self.log_scroll = (self.log_scroll as i32 + rows as i32).max(0) as usize;
            self.dirty = true;
        }
    }

    pub(super) fn menu_key(&mut self, k: KeyEvent) {
        let Some(m) = self.menu.as_mut() else { return };
        self.dirty = true;
        match m.page {
            Page::Achievements | Page::Creations if matches!(k.code, KeyCode::F(3)) => self.menu = None,
            Page::Creations if k.code == KeyCode::Tab => self.creations_key(k),
            _ if k.code == KeyCode::F(10) => self.menu = None,
            _ => self.ui.key(k),
        }
    }

    /// What the settings screen shows of the game around it.
    fn live(&self) -> crate::ui::settings::Live {
        let s = &self.settings;
        let note = |v: String| (v != "on" && v != "off").then(|| cap(&v));
        crate::ui::settings::Live {
            spent: self.llm.is_some().then(|| self.spent()),
            sound_note: (s.sound && self.sound.is_none() && !cfg!(test)).then(|| "No sound device found.".into()),
            voices_note: note(self.voice_status()),
            ears_note: if s.speak_to_talk { Some(cap(&self.ears_status())) } else { None },
            detail_now: self.win.is_some().then(|| self.detail_now()),
            together: self.together.is_some(),
        }
    }

    /// A setting of this world only (kept in its kv table, like `sim.<key>`).
    fn set_world(&mut self, key: &str, v: f64) {
        self.sim.cfg.set(key, v as f32);
        let _ = self.db.kv_set(&format!("sim.{key}"), &v.to_string());
    }

    /// The menu over the top `vh` rows (`over_world`: the world shows there, dimmed).
    pub(super) fn draw_menu(&mut self, w: u16, vh: u16, over_world: bool) {
        let Some(page) = self.menu.as_ref().map(|m| m.page) else { return };
        // The world, dimmed, behind the buttons; its names and marks hidden.
        self.screen.labels.clear();
        if self.win.is_some() && over_world {
            self.screen.fill_world(vh);
        } else {
            for y in 0..vh {
                for x in 0..w {
                    self.screen.over(x, y, |c| crate::term::Cell { ch: ' ', ..c });
                }
            }
        }
        let decor = self.screen.decor.len();
        self.screen.deco(crate::term::Deco { x: 0.0, y: 0.0, w: w as f32, h: vh as f32, fill: [6, 7, 12], alpha: 0.55, radius: 0.0, shadow: false, border: None });
        let mut ui = std::mem::take(&mut self.ui);
        ui.begin();
        let mut s = std::mem::replace(&mut self.screen, crate::term::Screen::new(1, 1, true));
        match page {
            Page::Pause => self.pause(&mut ui, &mut s, w, vh),
            Page::Settings => {
                let pw = 90.0f32.min(w as f32 - 2.0);
                let area = R::new((w as f32 - pw) * 0.5, 1.0, pw, vh as f32 - 2.0);
                let live = self.live();
                let mut st = self.settings.clone();
                let out = self.menu.as_mut().map(|m| m.settings.draw(&mut ui, &mut s, area, &mut st, &live)).unwrap_or_default();
                self.settings = st;
                self.settings_changed(out);
                if out.details {
                    let d = (self.db.usage_total(), self.db.usage_recent(10));
                    if let Some(m) = self.menu.as_mut() {
                        m.details = Some(d);
                    }
                    self.menu_page(Page::Details);
                } else if out.done {
                    self.menu_page(Page::Pause);
                }
            }
            Page::World => self.world_page(&mut ui, &mut s, w, vh),
            Page::Open => self.open_page(&mut ui, &mut s, w, vh),
            Page::Achievements => self.achievements_page(&mut ui, &mut s, w, vh),
            Page::Creations => self.creations_page(&mut ui, &mut s, w, vh),
            Page::Invite => self.invite_page(&mut ui, &mut s, w, vh),
            Page::Details => self.details_page(&mut ui, &mut s, w, vh),
        }
        ui.end();
        if ui.acted {
            self.dirty = true;
        }
        self.screen = s;
        // A new page starts with the keys on its first control, drawn at once.
        let fresh = self.menu.as_ref().is_some_and(|m| m.fresh);
        if fresh {
            ui.reset();
            ui.focus = self.menu.as_ref().map(|m| first_focus(m.page));
            if let Some(m) = self.menu.as_mut() {
                m.fresh = false;
            }
        }
        self.ui = ui;
        if fresh && self.menu.is_some() {
            self.screen.decor.truncate(decor);
            self.screen.labels.truncate(0);
            self.screen.over_decor.clear();
            self.screen.over_labels.clear();
            self.draw_menu(w, vh, over_world);
        }
    }

    fn pause(&mut self, ui: &mut crate::ui::Ui, s: &mut crate::term::Screen, w: u16, vh: u16) {
        let (w, h) = (w as f32, vh as f32);
        let cx = w * 0.5;
        let bw = 36.0f32.min(w - 4.0);
        let x = cx - bw * 0.5;
        let name = self.db.universe().ok().and_then(|u| serde_json::from_str::<serde_json::Value>(&u.look_json).ok()).and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string)).unwrap_or_default();
        let bh = 2.2;
        let gap = 0.7;
        let hosting = matches!(self.together, Some(super::together::Together::Host(_)));
        let can_open = self.together.is_none() && self.path.is_some() && self.win.is_some();
        let n = 6.0 + (hosting || can_open) as i32 as f32;
        let mut y = ((h - (n * (bh + gap) + 4.0)) * 0.5).max(0.5);
        s.title(cx, y + 1.4, "Paused", 2.8, ACCENT);
        // The world's name, and what has been spent on words this time.
        let spent = self.brain.has_llm.then(|| format!("${:.2} spent this session", self.spent()));
        let line = [Some(name).filter(|n| !n.is_empty()), spent].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        if !line.is_empty() {
            crate::ui::note(s, cx, y + 3.3, &line, DIM);
        }
        y += 4.6;
        let row = |y: f32| R::new(x, y, bw, bh);
        if ui.button(s, row(y), "Back to game", Kind::Primary) || ui.back() {
            self.menu = None;
            self.dirty = true;
            return;
        }
        y += bh + gap;
        let two = row(y).cols(2, 0.7);
        if ui.button(s, two[0], &format!("Achievements {}/{}", self.achievements.count(), achievements::ALL.len()), Kind::Normal) {
            return self.menu_page(Page::Achievements);
        }
        if ui.button(s, two[1], "Creations", Kind::Normal) {
            return self.menu_page(Page::Creations);
        }
        y += bh + gap;
        let two = row(y).cols(2, 0.7);
        if ui.button(s, two[0], "World options", Kind::Normal) {
            return self.menu_page(Page::World);
        }
        if ui.button(s, two[1], "Settings", Kind::Normal) {
            return self.menu_page(Page::Settings);
        }
        y += bh + gap;
        if hosting {
            let n = match &self.together {
                Some(super::together::Together::Host(h)) => h.guests(),
                _ => 0,
            };
            let label = if n == 0 { "Invite friends".to_string() } else { format!("Invite friends · {n} here") };
            if ui.button(s, row(y), &label, Kind::Normal) {
                return self.menu_page(Page::Invite);
            }
            y += bh + gap;
        } else if can_open {
            if ui.button(s, row(y), "Open to friends", Kind::Normal) {
                return self.menu_page(Page::Open);
            }
            y += bh + gap;
        }
        let quit_label = if self.is_guest() { "Leave and go to title" } else { "Save and quit to title" };
        if ui.button(s, row(y), quit_label, Kind::Normal) {
            self.to_worlds = true;
            self.quit = true;
            return;
        }
        y += bh + gap;
        if ui.button(s, row(y), "Quit game", Kind::Quiet) || ui.letter('q') {
            self.quit = true;
        }
    }

    fn world_page(&mut self, ui: &mut crate::ui::Ui, s: &mut crate::term::Screen, w: u16, vh: u16) {
        use crate::ui::settings::{ROW, Rows, step};
        let pw = 76.0f32.min(w as f32 - 2.0);
        let area = R::new((w as f32 - pw) * 0.5, 1.0, pw, (vh as f32 - 2.0).min(24.0));
        s.deco(crate::term::Deco::panel(area.x, area.y, area.w, area.h, crate::ui::PANEL).border(crate::ui::PANEL_EDGE));
        s.labels.push(crate::ui::left(area.x + 2.0, area.y + 2.0, "World options", 1.8, ACCENT, crate::term::Face::Title));
        let mut rows = Rows { x: area.x + 1.0, y: area.y + 3.8, w: area.w - 2.0, top: area.y + 3.0, bottom: area.y + area.h - 3.0 };
        let levels = crate::sim::night::LEVELS;
        let r = rows.next(ROW);
        let d = ui.choice(s, r, "Difficulty", &cap(self.sim.level().name), false);
        if d != 0 {
            let v = (self.sim.cfg.difficulty as i32 + d).clamp(0, levels.len() as i32 - 1);
            self.set_world("difficulty", v as f64);
        }
        rows.head(s, "Life in this world");
        let r = rows.next(ROW);
        let d = ui.choice(s, r, "Most of one kind of creature", &format!("{} a region", self.sim.cfg.max_creatures), false);
        if d != 0 {
            self.set_world("max_creatures", step(CREATURES, self.sim.cfg.max_creatures as f64, d));
        }
        let r = rows.next(ROW);
        let d = ui.choice(s, r, "How fast lives go", &format!("×{}", self.sim.cfg.life_speed), false);
        if d != 0 {
            self.set_world("life_speed", step(LIFE, self.sim.cfg.life_speed as f64, d));
        }
        let r = rows.next(ROW);
        let mut v = self.sim.cfg.hunting;
        if ui.toggle(s, r, "Hunters kill their prey", &mut v, false) {
            self.set_world("hunting", v as i32 as f64);
        }
        let r = rows.next(ROW);
        let mut v = self.sim.cfg.create_beings;
        if ui.toggle(s, r, "Actions can bring new beings", &mut v, false) {
            self.set_world("create_beings", v as i32 as f64);
        }
        let done = R::new(area.x + area.w - 14.0, area.y + area.h - 2.8, 12.0, 2.0);
        if ui.button(s, done, "Done", Kind::Primary) || ui.back() {
            self.menu_page(Page::Pause);
        }
    }

    /// A panel in the view, `pw` wide and `ph` tall at most, titled; the rect.
    pub(super) fn page_panel(s: &mut crate::term::Screen, w: u16, vh: u16, pw: f32, ph: f32, title: &str) -> R {
        let pw = pw.min(w as f32 - 2.0);
        let ph = ph.min(vh as f32 - 2.0);
        let area = R::new((w as f32 - pw) * 0.5, ((vh as f32 - ph) * 0.5).max(1.0), pw, ph);
        s.deco(crate::term::Deco::panel(area.x, area.y, area.w, area.h, crate::ui::PANEL).border(crate::ui::PANEL_EDGE));
        s.labels.push(crate::ui::left(area.x + 2.0, area.y + 2.0, title, 1.8, ACCENT, crate::term::Face::Title));
        area
    }

    /// Words wrapped in the panel; the row after them.
    fn page_para(s: &mut crate::term::Screen, area: R, mut y: f32, text: &str, color: [u8; 3]) -> f32 {
        for l in crate::term::wrap(text, ((area.w - 4.0) / 0.8) as usize) {
            s.labels.push(crate::ui::left(area.x + 2.0, y + 0.5, &l, 0.95, color, crate::term::Face::Body));
            y += 1.15;
        }
        y
    }

    fn open_page(&mut self, ui: &mut crate::ui::Ui, s: &mut crate::term::Screen, w: u16, vh: u16) {
        let area = Self::page_panel(s, w, vh, 70.0, 20.0, "Open to friends");
        let mut y = area.y + 3.8;
        y = Self::page_para(s, area, y, "Friends join this world and play in it with you, right now: you keep playing while they arrive.", TEXT) + 0.5;
        y = Self::page_para(s, area, y, "What they say and make uses your AI, up to what you allow each guest in Settings → Together. They need no key and no copy of the world.", DIM) + 0.5;
        let ngrok = crate::net::ngrok::find().is_some();
        if !ngrok {
            y = Self::page_para(s, area, y, "For friends outside this network, install ngrok (ngrok.com/download) and sign in once.", crate::ui::FAINT);
        }
        if let Some(e) = self.menu.as_ref().and_then(|m| m.open_err.clone()) {
            Self::page_para(s, area, y + 0.4, &format!("Couldn't open it: {e}"), [240, 150, 130]);
        }
        let row = R::new(area.x + 2.0, area.y + area.h - 5.6, area.w - 4.0, 2.1).cols(2, 0.8);
        let mut internet = None;
        if ui.button(s, row[0], "On this network", Kind::Primary) {
            internet = Some(false);
        }
        if ui.button_on(s, row[1], "For anyone (ngrok)", Kind::Normal, ngrok) {
            internet = Some(true);
        }
        let back = R::new(area.x + area.w - 14.0, area.y + area.h - 3.0, 12.0, 2.0);
        if ui.button(s, back, "Back", Kind::Quiet) || ui.back() {
            return self.menu_page(Page::Pause);
        }
        if let Some(internet) = internet {
            match self.open_to_friends(internet) {
                Ok(()) => self.menu_page(Page::Invite),
                Err(e) => {
                    if let Some(m) = self.menu.as_mut() {
                        m.open_err = Some(format!("{e:#}"));
                    }
                }
            }
        }
    }

    /// Start hosting this world as it is being played.
    fn open_to_friends(&mut self, internet: bool) -> anyhow::Result<()> {
        use anyhow::Context;
        let path = self.path.clone().context("this world has no file to share")?;
        let (me, _) = self.settings.player();
        let host = crate::net::host::Host::start(crate::net::PORT, crate::net::new_code())?;
        let name = if self.snap.look.name.is_empty() { "this world".to_string() } else { self.snap.look.name.clone() };
        self.together = Some(super::together::Together::Host(super::together::Hosting::new(host, path, name, me, internet)));
        self.say(None, "Open to friends: they can join now (Esc → Invite friends for the invite).", ACCENT);
        Ok(())
    }

    fn invite_page(&mut self, ui: &mut crate::ui::Ui, s: &mut crate::term::Screen, w: u16, vh: u16) {
        let Some(super::together::Together::Host(h)) = &self.together else { return self.menu_page(Page::Pause) };
        let shown = self.menu.as_ref().is_some_and(|m| m.show_addr);
        let (addr, code, names) = (h.address(), h.host.code.clone(), h.names());
        let addr = if shown { addr } else { crate::ui::secret_view(&addr) };
        let area = Self::page_panel(s, w, vh, 70.0, 22.0, "Invite friends");
        let mut y = area.y + 3.8;
        y = Self::page_para(s, area, y, "Send them this invite (the address stays hidden on screen; Copy invite still copies it). In Infinite Pocket they choose Play together → Join with an invite, and paste it. On this network your game also shows there by itself.", DIM) + 0.8;
        let card = R::new(area.x + 2.0, y, area.w - 4.0, 4.4);
        s.deco(crate::term::Deco::panel(card.x, card.y, card.w, card.h, [18, 20, 28]).border(crate::ui::PANEL_EDGE));
        s.labels.push(crate::ui::left(card.x + 1.5, card.y + 1.2, "Address", 0.85, DIM, crate::term::Face::Body));
        s.labels.push(crate::ui::left(card.x + 12.0, card.y + 1.2, &addr, 1.0, TEXT, crate::term::Face::Bold));
        let eye = R::new(card.x + card.w - 9.5, card.y + 0.25, 8.0, 1.8);
        if ui.button(s, eye, if shown { "Hide" } else { "Show" }, Kind::Quiet) {
            if let Some(m) = self.menu.as_mut() {
                m.show_addr = !shown;
            }
        }
        s.labels.push(crate::ui::left(card.x + 1.5, card.y + 3.0, "Code", 0.85, DIM, crate::term::Face::Body));
        s.labels.push(crate::ui::left(card.x + 12.0, card.y + 3.0, &code, 1.3, ACCENT, crate::term::Face::Bold));
        y += 5.4;
        let who = if names.is_empty() { "Nobody has joined yet.".to_string() } else { format!("Here with you: {}.", names.join(", ")) };
        Self::page_para(s, area, y, &who, TEXT);
        let row = R::new(area.x + 2.0, area.y + area.h - 3.0, area.w - 4.0, 2.1).cols(3, 0.8);
        if ui.button(s, row[0], "Stop hosting", Kind::Danger) {
            self.together_end();
            self.say(None, "Closed to friends: it's just you again.", DIM);
            return self.menu_page(Page::Pause);
        }
        if ui.button(s, row[1], "Back", Kind::Quiet) || ui.back() {
            return self.menu_page(Page::Pause);
        }
        if ui.button(s, row[2], "Copy invite", Kind::Primary) {
            self.copy_invite();
        }
    }

    fn details_page(&mut self, ui: &mut crate::ui::Ui, s: &mut crate::term::Screen, w: u16, vh: u16) {
        let Some((total, recent)) = self.menu.as_ref().and_then(|m| m.details.clone()) else { return self.menu_page(Page::Settings) };
        let pw = 76.0f32.min(w as f32 - 2.0);
        let area = R::new((w as f32 - pw) * 0.5, 1.0, pw, (vh as f32 - 2.0).min(24.0));
        s.deco(crate::term::Deco::panel(area.x, area.y, area.w, area.h, crate::ui::PANEL).border(crate::ui::PANEL_EDGE));
        s.labels.push(crate::ui::left(area.x + 2.0, area.y + 2.0, "What the AI cost", 1.8, ACCENT, crate::term::Face::Title));
        let mut y = area.y + 4.0;
        let line = |s: &mut crate::term::Screen, y: f32, a: &str, b: &str, c: [u8; 3]| {
            s.labels.push(crate::ui::left(area.x + 2.5, y, a, 0.95, c, crate::term::Face::Body));
            s.labels.push(crate::ui::left(area.x + area.w - 14.0, y, b, 0.95, c, crate::term::Face::Body));
        };
        let budget = self.settings.budget().map(|b| format!(" of ${b:.2}")).unwrap_or_default();
        line(s, y, "This session", &format!("${:.2}{budget}", self.spent()), TEXT);
        y += 1.2;
        line(s, y, "This world, all time", &format!("${total:.2}"), TEXT);
        y += 2.0;
        let now = crate::db::now();
        if recent.is_empty() {
            line(s, y, "No calls yet.", "", DIM);
        }
        for (t, p, c) in &recent {
            if y > area.y + area.h - 4.0 {
                break;
            }
            line(s, y, &format!("{} · {}", cap(purpose(p)), ago(now - t)), &format!("${c:.3}"), DIM);
            y += 1.15;
        }
        let done = R::new(area.x + area.w - 14.0, area.y + area.h - 2.8, 12.0, 2.0);
        if ui.button(s, done, "Back", Kind::Primary) || ui.back() {
            self.menu_page(Page::Settings);
        }
    }

    /// Act on settings just changed: sound, the mic, the budget.
    fn settings_changed(&mut self, out: crate::ui::settings::Out) {
        if out.reset_spent {
            if let Some(l) = &self.llm {
                *l.spent.lock() = 0.0;
            }
        }
        if out.ears {
            self.sync_ears();
            if self.settings.speak_to_talk {
                self.ask_mic();
            }
        }
        if out.sound {
            self.sync_sound();
        }
        if out.ai {
            self.say(None, "The AI settings take effect the next time this world opens.", DIM);
        }
        if let Some(l) = &self.llm {
            *l.budget.lock() = self.settings.budget();
            if self.budget_paused && !l.over_budget() {
                self.budget_paused = false;
                self.say(None, "Budget raised: generation and dialogue are back on.", DIM);
            }
        }
    }

    /// The achievements page: every one as a card, by group, with its medal;
    /// how many are earned at the top. ↑↓ or the wheel move through them.
    fn achievements_page(&mut self, ui: &mut crate::ui::Ui, s: &mut crate::term::Screen, w: u16, vh: u16) {
        use crate::term::{Deco, Face, Label};
        use crate::ui::{FAINT, left};
        let total = achievements::ALL.len();
        let got = self.achievements.count();
        let area = Self::page_panel(s, w, vh, 96.0, 200.0, "Achievements");
        // Progress, top right.
        let bw = 22.0f32.min(area.w * 0.3);
        let bx = area.x + area.w - bw - 15.0;
        s.deco(Deco::pill(bx, area.y + 1.8, bw, 0.5, [44, 48, 66]));
        s.deco(Deco::pill(bx, area.y + 1.8, (bw * got as f32 / total as f32).max(0.5), 0.5, ACCENT));
        s.labels.push(Label { x: bx + bw * 0.5, y: area.y + 1.0, text: format!("{got} of {total} earned"), size: 0.85, color: DIM, face: Face::Body, center: true });
        let back = R::new(area.x + area.w - 12.0, area.y + 1.0, 10.0, 1.8);
        if ui.button(s, back, "Back", Kind::Quiet) || ui.back() {
            return self.menu_page(Page::Pause);
        }
        // The cards: two across when there is room.
        let cols = if area.w >= 80.0 { 2 } else { 1 };
        let gap = 0.8;
        let cw = (area.w - 4.0 - gap * (cols as f32 - 1.0)) / cols as f32;
        let ch = 3.6;
        let top = area.y + 4.0;
        let bottom = area.y + area.h - 1.0;
        let Some(m) = self.menu.as_mut() else { return };
        let mut sel = m.ach_sel.min(total - 1);
        let (_, focused) = ui.list(R::new(area.x, top, area.w, bottom - top), total, &mut sel);
        // Lay out: a heading per group, then its cards in rows.
        let mut place = Vec::with_capacity(total);
        let mut heads = Vec::new();
        let mut y = 0.0f32;
        let mut group = "";
        let mut col = 0;
        for d in achievements::ALL.iter() {
            if d.group != group {
                if col > 0 {
                    y += ch + gap;
                }
                group = d.group;
                heads.push((y, group));
                y += 1.9;
                col = 0;
            }
            place.push((area.x + 2.0 + col as f32 * (cw + gap), y));
            col += 1;
            if col == cols {
                col = 0;
                y += ch + gap;
            }
        }
        let len = y + if col > 0 { ch } else { 0.0 };
        // Scroll: the wheel (or two fingers), and the chosen card kept in
        // view when the keys move it.
        let view = bottom - top;
        m.ach_scroll = (m.ach_scroll - ui.wheel * 3.0).clamp(0.0, (len - view).max(0.0));
        if sel != m.ach_sel {
            let sy = place[sel].1;
            if sy < m.ach_scroll {
                m.ach_scroll = (sy - 2.0).max(0.0);
            } else if sy + ch > m.ach_scroll + view {
                m.ach_scroll = sy + ch - view;
            }
        }
        m.ach_sel = sel;
        let scroll = m.ach_scroll;
        let shown = |y: f32, h: f32| y - scroll >= -0.01 && y - scroll + h <= view + 0.01;
        for (hy, g) in heads {
            if shown(hy, 1.9) {
                s.labels.push(left(area.x + 2.2, top + hy - scroll + 1.0, g, 1.15, HEAD, Face::Title));
            }
        }
        let now = crate::db::now();
        for (i, d) in achievements::ALL.iter().enumerate() {
            let (x, y) = place[i];
            if !shown(y, ch) {
                continue;
            }
            let r = R::new(x, top + y - scroll, cw, ch);
            let earned = self.achievements.earned(d.id);
            let it = ui.card(s, r, i == sel && focused, focused);
            if it.click {
                if let Some(m) = self.menu.as_mut() {
                    m.ach_sel = i;
                }
            }
            medal(s, r.x + 1.1, r.y + 0.6, 2.4, d.tier, earned.is_some());
            let tx = r.x + 7.0;
            let tw = r.w - 8.0;
            s.labels.push(left(tx, r.y + 0.95, &crate::ui::fit(d.title, tw - 10.0, 1.0), 1.0, if earned.is_some() { TEXT } else { DIM }, Face::Bold));
            let when = match earned {
                Some(t) => ago(now - t),
                None => tier_name(d.tier).to_string(),
            };
            s.labels.push(Label { x: r.x + r.w - 1.2 - when.chars().count() as f32 * 0.72, y: r.y + 0.5, text: when, size: 0.8, color: if earned.is_some() { tier_color(d.tier) } else { FAINT }, face: Face::Body, center: false });
            for (k, l) in crate::term::wrap(d.text, (tw / 0.72) as usize).iter().take(2).enumerate() {
                s.labels.push(left(tx, r.y + 2.05 + k as f32 * 0.95, l, 0.82, if earned.is_some() { DIM } else { FAINT }, Face::Body));
            }
        }
    }
}


/// Where the keys start on a page: its list, on the pages that are one.
fn first_focus(p: Page) -> usize {
    match p {
        // After Back (and the creations' three groups).
        Page::Achievements => 1,
        Page::Creations => 4,
        _ => 0,
    }
}

pub(super) fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}
