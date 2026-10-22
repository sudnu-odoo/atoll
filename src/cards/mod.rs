pub mod clock;
pub mod lyrics;
pub mod music;
pub mod peek;
pub mod water;
pub mod widgets;

use gtk::glib;

/// Calls `f` now and then at every minute boundary. Waking once per minute,
/// aligned, keeps the clock exact without any per-second timer.
pub fn every_minute(f: impl Fn(&glib::DateTime) + 'static) {
    fn schedule(f: std::rc::Rc<dyn Fn(&glib::DateTime)>) {
        let Ok(now) = glib::DateTime::now_local() else {
            return;
        };
        f(&now);
        let wait_ms = 60_000 - u64::from(now.second().unsigned_abs()) * 1000
            + 50 // land just after the boundary, never just before
            - u64::try_from(now.microsecond() / 1000).unwrap_or(0);
        glib::timeout_add_local_once(std::time::Duration::from_millis(wait_ms), move || {
            schedule(f)
        });
    }
    schedule(std::rc::Rc::new(f));
}

/// Formats a duration as m:ss, the way music players do.
pub fn mmss(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// Keeps `label` showing the current local time in `format`, ticking once a
/// second, aligned to the second, but only while the label is on screen.
/// Expanded cards use it for seconds; collapsed, nothing ticks.
pub fn live_clock(label: &gtk::Label, format: &'static str) {
    use gtk::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn tick(label: &gtk::Label, format: &str) -> Option<std::time::Duration> {
        let now = glib::DateTime::now_local().ok()?;
        if let Ok(s) = now.format(format) {
            label.set_label(&s);
        }
        let ms_into_second = u64::try_from(now.microsecond() / 1000).unwrap_or(0);
        Some(std::time::Duration::from_millis(1000 - ms_into_second + 5))
    }

    fn schedule(
        label: &gtk::Label,
        format: &'static str,
        id: &Rc<RefCell<Option<glib::SourceId>>>,
    ) {
        let Some(wait) = tick(label, format) else {
            return;
        };
        let (weak, slot) = (label.downgrade(), id.clone());
        let next = glib::timeout_add_local_once(wait, move || {
            slot.take();
            if let Some(label) = weak.upgrade() {
                schedule(&label, format, &slot);
            }
        });
        id.replace(Some(next));
    }

    let id: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let slot = id.clone();
    label.connect_map(move |label| schedule(label, format, &slot));
    label.connect_unmap(move |_| {
        if let Some(source) = id.take() {
            source.remove();
        }
    });
}

/// A notch button: never takes keyboard focus (clicking the island must not
/// steal focus from the user's window) and shows the hand cursor.
pub fn control_button(classes: &[&str], icon: Option<&str>) -> gtk::Button {
    use gtk::prelude::*;
    let mut all = vec!["control"];
    all.extend_from_slice(classes);
    let b = gtk::Button::builder()
        .css_classes(all)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .focus_on_click(false)
        .can_focus(false)
        .build();
    if let Some(icon) = icon {
        b.set_icon_name(icon);
    }
    b.set_cursor_from_name(Some("pointer"));
    b
}
