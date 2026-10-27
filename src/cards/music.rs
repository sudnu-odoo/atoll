//! Expanded music card, with an optional lyrics panel. Renders `NowPlaying`
//! and lyrics it is given, and sends `Command`s; it never does I/O itself.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib, Align, Button, Label, Orientation};

use super::lyrics::LyricsView;
use super::widgets::{Cover, Meter, Ring, TrackCell};
use super::{every_minute, live_clock, mmss};
use crate::services::lyrics::Line;
use crate::services::mpris::{Command, NowPlaying};

// Bundled Material Symbols (resources/icons). SVG icons are centered in
// their box; font glyphs carry side bearings that push them off-center.
const ICON_PREV: &str = "atoll-previous-symbolic";
const ICON_NEXT: &str = "atoll-next-symbolic";
const ICON_PLAY: &str = "atoll-play-symbolic";
const ICON_PAUSE: &str = "atoll-pause-symbolic";
const ICON_LYRICS: &str = "atoll-lyrics-symbolic";
const ICON_PIN: &str = "atoll-pin-symbolic";
const ICON_PINNED: &str = "atoll-pinned-symbolic";

/// The "clickable" hand cursor from the user's cursor theme.
const HAND: &str = "pointer";

const COVER: i32 = 80;
/// Player column width in lyrics mode; the lyrics take the rest.
const PLAYER_W: i32 = 380;

/// Progress only needs a second; lyrics need to catch line changes promptly.
const TICK: Duration = Duration::from_secs(1);
const TICK_LYRICS: Duration = Duration::from_millis(200);
/// A drag produces a stream of values; seek once it pauses or ends.
const SEEK_DEBOUNCE: Duration = Duration::from_millis(180);
/// If the player never confirms a seek, stop overriding its position.
const SEEK_HOLD: Duration = Duration::from_millis(1500);

pub enum Lyrics {
    Loading,
    Missing,
    /// LRCLIB couldn't be reached; reopening the panel retries.
    Unreachable,
    /// The lines, and whether their timing is real (else evenly paced).
    Lines(Vec<Line>, bool),
}

pub struct MusicCard {
    pub root: gtk::Box,
    player: gtk::Box,
    top: gtk::CenterBox,
    cover: Cover,
    title: Label,
    subtitle: Label,
    elapsed: Label,
    remaining: Label,
    progress: gtk::Scale,
    commands: async_channel::Sender<Command>,
    /// Fraction the user dragged to; shown instead of the player's position
    /// until the player reports back.
    seek_to: Cell<Option<f64>>,
    seek_timer: RefCell<Option<glib::SourceId>>,
    play: Button,
    lyrics_button: Button,
    pin_button: Button,
    lyrics_panel: gtk::Box,
    lyrics_view: Rc<LyricsView>,
    /// Shown under plain lyrics, whose timing is only estimated.
    unsynced: Label,
    ring: Rc<Ring>,
    meter: Rc<Meter>,
    track: TrackCell,
    lyrics: RefCell<Lyrics>,
    lyrics_mode: Cell<bool>,
    active: Cell<bool>,
    ticker: RefCell<Option<glib::SourceId>>,
}

impl MusicCard {
    pub fn new(
        commands: async_channel::Sender<Command>,
        on_lyrics: impl Fn() + 'static,
        on_pin: impl Fn() + 'static,
    ) -> Rc<Self> {
        let track = TrackCell::default();

        // Top row sits under the GNOME bar, where the Shell receives all
        // clicks, so it only shows information; every control is lower.
        let ring = Ring::new(track.clone(), 14);
        let date = Label::builder().css_classes(["date"]).build();
        let time = Label::builder().css_classes(["clock"]).build();
        // The meter rides with the time; the top-right corner belongs to
        // the settings button.
        let meter = Meter::new();
        let when = gtk::Box::builder().spacing(8).build();
        when.append(&ring.area);
        when.append(&date);
        when.append(&time);
        when.append(&meter.area);
        let top = gtk::CenterBox::builder()
            .css_classes(["card-top"])
            .hexpand(true)
            .center_widget(&when)
            .build();
        live_clock(&time, "%H:%M:%S");
        every_minute(move |now| {
            if let Ok(s) = now.format("%a %d %b %Y") {
                date.set_label(&s);
            }
        });

        let cover = Cover::new(COVER, 14.0);
        let text_label = |class: &str| {
            Label::builder()
                .css_classes([class])
                // Fill (text still left-aligned by xalign): its natural width
                // is tiny on purpose, see below.
                .halign(Align::Fill)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                // A small natural width: a long title then ellipsizes in its
                // column instead of widening the player over the lyrics.
                .max_width_chars(1)
                .hexpand(true)
                .build()
        };
        let title = text_label("title");
        let subtitle = text_label("subtitle");
        let text = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .valign(Align::Center)
            .hexpand(true)
            .spacing(3)
            .build();
        text.append(&title);
        text.append(&subtitle);

        // The cover spans the top row and the title, like a header block.
        let header = gtk::Grid::builder().column_spacing(18).build();
        header.attach(&cover.area, 0, 0, 1, 2);
        header.attach(&top, 1, 0, 1, 1);
        header.attach(&text, 1, 1, 1, 1);

        let elapsed = Label::builder().css_classes(["time"]).xalign(0.0).build();
        let remaining = Label::builder().css_classes(["time"]).xalign(1.0).build();
        let progress = gtk::Scale::builder()
            .orientation(Orientation::Horizontal)
            .adjustment(&gtk::Adjustment::new(0.0, 0.0, 1.0, 0.01, 0.05, 0.0))
            .draw_value(false)
            .hexpand(true)
            .valign(Align::Center)
            .focus_on_click(false)
            .can_focus(false)
            .build();
        progress.set_cursor_from_name(Some(HAND));
        let bar = gtk::Box::builder().css_classes(["bar"]).spacing(14).build();
        bar.append(&elapsed);
        bar.append(&progress);
        bar.append(&remaining);

        let button = |classes: &[&str]| {
            let mut all = vec!["control"];
            all.extend_from_slice(classes);
            let b = Button::builder()
                .css_classes(all)
                .halign(Align::Center)
                .valign(Align::Center)
                // Clicking must never pull keyboard focus onto the island.
                .focus_on_click(false)
                .can_focus(false)
                .build();
            b.set_cursor_from_name(Some(HAND));
            b
        };
        let send = |b: &Button, cmd: fn() -> Command| {
            let tx = commands.clone();
            b.connect_clicked(move |_| {
                let _ = tx.try_send(cmd());
            });
        };
        let prev = button(&[]);
        prev.set_icon_name(ICON_PREV);
        let play = button(&["primary"]);
        play.set_icon_name(ICON_PLAY);
        let next = button(&[]);
        next.set_icon_name(ICON_NEXT);
        send(&prev, || Command::Previous);
        send(&play, || Command::PlayPause);
        send(&next, || Command::Next);

        let lyrics_button = button(&["round"]);
        lyrics_button.set_icon_name(ICON_LYRICS);
        lyrics_button.set_tooltip_text(Some("Lyrics"));
        lyrics_button.connect_clicked(move |_| on_lyrics());
        let pin_button = button(&["round"]);
        pin_button.set_icon_name(ICON_PIN);
        pin_button.set_tooltip_text(Some("Keep open"));
        pin_button.connect_clicked(move |_| on_pin());

        let transport = gtk::Box::builder().spacing(16).build();
        transport.append(&prev);
        transport.append(&play);
        transport.append(&next);
        let controls = gtk::CenterBox::builder()
            .css_classes(["controls"])
            .start_widget(&lyrics_button)
            .center_widget(&transport)
            .end_widget(&pin_button)
            .build();

        let player = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["player"])
            .hexpand(true)
            .build();
        player.append(&header);
        player.append(&bar);
        player.append(&controls);

        let lyrics_view = LyricsView::new();
        let lyrics_panel = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["lyrics"])
            .hexpand(true)
            .visible(false)
            .build();
        lyrics_view.root.set_vexpand(true);
        lyrics_panel.append(&lyrics_view.root);
        let unsynced = Label::builder()
            .label("NOT SYNCED · PACED OVER THE SONG")
            .css_classes(["caps", "muted"])
            .xalign(0.0)
            .visible(false)
            .build();
        lyrics_panel.append(&unsynced);

        let root = gtk::Box::builder()
            .css_classes(["music-card"])
            .valign(Align::Start)
            .build();
        root.append(&player);
        root.append(&lyrics_panel);

        let card = Rc::new(Self {
            root,
            player,
            top,
            cover,
            title,
            subtitle,
            elapsed,
            remaining,
            progress,
            commands: commands.clone(),
            seek_to: Cell::new(None),
            seek_timer: RefCell::new(None),
            play,
            lyrics_button,
            pin_button,
            lyrics_panel,
            lyrics_view,
            unsynced,
            ring,
            meter,
            track,
            lyrics: RefCell::new(Lyrics::Missing),
            lyrics_mode: Cell::new(false),
            active: Cell::new(false),
            ticker: RefCell::new(None),
        });
        card.set_lyrics_mode(false);

        // `change-value` fires only for user input (click or drag), never for
        // our own set_value calls, so playback updates can't loop into seeks.
        let weak = Rc::downgrade(&card);
        card.progress.connect_change_value(move |_, _, value| {
            if let Some(card) = weak.upgrade() {
                card.preview_seek(value.clamp(0.0, 1.0));
            }
            glib::Propagation::Proceed
        });
        card
    }

    fn preview_seek(self: &Rc<Self>, fraction: f64) {
        let has_length = self
            .track
            .borrow()
            .as_ref()
            .and_then(|np| np.length)
            .is_some();
        if !has_length {
            return; // live streams can't seek
        }
        self.seek_to.set(Some(fraction));
        self.refresh();
        if let Some(id) = self.seek_timer.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(SEEK_DEBOUNCE, move || {
            if let Some(card) = weak.upgrade() {
                card.seek_timer.take();
                card.commit_seek();
            }
        });
        self.seek_timer.replace(Some(id));
    }

    fn commit_seek(self: &Rc<Self>) {
        let (Some(fraction), Some(len)) = (
            self.seek_to.get(),
            self.track.borrow().as_ref().and_then(|np| np.length),
        ) else {
            return;
        };
        let _ = self
            .commands
            .try_send(Command::SetPosition(len.mul_f64(fraction)));
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(SEEK_HOLD, move || {
            if let Some(card) = weak.upgrade() {
                card.seek_timer.take();
                card.seek_to.set(None);
                card.refresh();
            }
        });
        self.seek_timer.replace(Some(id));
    }

    pub fn update(self: &Rc<Self>, np: &NowPlaying) {
        self.title.set_label(&np.title);
        let sub: Vec<&str> = [np.artist.as_str(), np.album.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        self.subtitle.set_label(&sub.join(" - "));
        self.play
            .set_icon_name(if np.playing { ICON_PAUSE } else { ICON_PLAY });
        self.track.replace(Some(np.clone()));
        // A fresh snapshot after a seek carries the real position now.
        if self.seek_to.get().is_some() && self.seek_timer.borrow().is_none() {
            self.seek_to.set(None);
        }
        self.ring.refresh();
        self.meter.set_playing(np.playing);
        self.refresh();
        self.restart_ticker();
    }

    pub fn set_art(&self, art: Option<&gdk::Texture>) {
        self.cover.set(art);
    }

    /// Normal: date row on top, compact. Lyrics: player on the left (no date
    /// row), lyrics on the right; the island makes the notch wider.
    pub fn set_lyrics_mode(self: &Rc<Self>, on: bool) {
        self.lyrics_mode.set(on);
        self.top.set_visible(!on);
        self.lyrics_panel.set_visible(on);
        self.player.set_hexpand(!on);
        self.player
            .set_width_request(if on { PLAYER_W } else { -1 });
        let (w, h) = if on {
            crate::island::EXPANDED_LYRICS
        } else {
            crate::island::EXPANDED_MUSIC
        };
        self.root.set_size_request(w as i32, h as i32);
        toggle_class(&self.lyrics_button, "on", on);
        toggle_class(&self.player, "with-lyrics", on);
        self.refresh();
        self.restart_ticker();
    }

    /// Lyrics switched off in settings: no button, no lookups.
    pub fn set_lyrics_available(&self, available: bool) {
        self.lyrics_button.set_visible(available);
    }

    pub fn set_pinned(&self, pinned: bool) {
        self.pin_button
            .set_icon_name(if pinned { ICON_PINNED } else { ICON_PIN });
        toggle_class(&self.pin_button, "on", pinned);
    }

    pub fn set_lyrics(&self, lyrics: Lyrics) {
        self.unsynced
            .set_visible(matches!(&lyrics, Lyrics::Lines(_, false)));
        let translated = matches!(&lyrics, Lyrics::Lines(lines, _)
            if lines.iter().any(|l| l.translation.is_some()));
        self.lyrics_view.set_translated(translated);
        self.lyrics.replace(lyrics);
        self.refresh();
    }

    /// Ticks only while the card is on screen; a hidden card costs nothing.
    pub fn set_active(self: &Rc<Self>, active: bool) {
        self.active.set(active);
        self.restart_ticker();
    }

    fn restart_ticker(self: &Rc<Self>) {
        if let Some(id) = self.ticker.take() {
            id.remove();
        }
        let playing = self.track.borrow().as_ref().is_some_and(|np| np.playing);
        if !self.active.get() || !playing {
            return;
        }
        self.refresh();
        let every = if self.lyrics_mode.get() {
            TICK_LYRICS
        } else {
            TICK
        };
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(every, move || match weak.upgrade() {
            Some(card) => {
                card.refresh();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        self.ticker.replace(Some(id));
    }

    fn refresh(&self) {
        let track = self.track.borrow();
        let Some(np) = track.as_ref() else {
            return;
        };
        let seeking = self.seek_to.get();
        let pos = match (seeking, np.length) {
            (Some(f), Some(len)) => len.mul_f64(f),
            _ => np.position_now(),
        };
        set_if_changed(&self.elapsed, &mmss(pos));
        match np.length {
            Some(len) => {
                set_if_changed(
                    &self.remaining,
                    &format!("-{}", mmss(len.saturating_sub(pos))),
                );
                // Never move the slider out from under the user's pointer.
                if seeking.is_none() {
                    self.progress
                        .set_value(pos.as_secs_f64() / len.as_secs_f64().max(1.0));
                }
            }
            // Live streams have no length.
            None => {
                set_if_changed(&self.remaining, "LIVE");
                self.progress.set_value(0.0);
            }
        }
        if self.lyrics_mode.get() {
            self.show_lyrics(pos);
        }
    }

    fn show_lyrics(&self, pos: Duration) {
        match &*self.lyrics.borrow() {
            Lyrics::Loading => self.lyrics_view.message("Looking for lyrics…"),
            Lyrics::Missing => self.lyrics_view.message("No lyrics found for this song"),
            Lyrics::Unreachable => self
                .lyrics_view
                .message("Couldn't reach LRCLIB · reopen lyrics to retry"),
            Lyrics::Lines(lines, _) => {
                // The line being sung: the last one that has started.
                let index = lines.partition_point(|l| l.at <= pos) as isize - 1;
                self.lyrics_view.show(index, |i| {
                    usize::try_from(i)
                        .ok()
                        .and_then(|i| lines.get(i))
                        .map(|l| (l.text.as_str(), l.translation.as_deref()))
                });
            }
        }
    }
}

fn toggle_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// `set_label` always relayouts; skip it when nothing changed.
fn set_if_changed(label: &Label, text: &str) {
    if label.label() != text {
        label.set_label(text);
    }
}
