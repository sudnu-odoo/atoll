pub mod clock;
pub mod lyrics;
pub mod music;
pub mod peek;
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
