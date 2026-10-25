//! Where printed text goes. Normally stdout; in full-screen mode a buffer the transcript pane draws, the way the
//! Python UI pointed its console at a Sink. Every `uprintln!` in the front end goes through here.
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

const MAX_LINES: usize = 3000;

#[derive(Default)]
pub struct Sink {
    pub text: String,
    /// Newlines in `text`, counted as they arrive.
    nl: usize,
    /// Bumps when the oldest lines are dropped, so cached parsing is thrown away.
    pub generation: u64,
}

fn slot() -> &'static Mutex<Option<Sink>> {
    static S: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static DIRTY: AtomicBool = AtomicBool::new(false);
static WIDTH: AtomicUsize = AtomicUsize::new(0);

pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

pub fn install() {
    *slot().lock().unwrap() = Some(Sink::default());
    ACTIVE.store(true, Ordering::Relaxed);
}

/// Leave full-screen mode; returns what was printed so it can be put on the real terminal.
pub fn uninstall() -> String {
    ACTIVE.store(false, Ordering::Relaxed);
    slot().lock().unwrap().take().map(|s| s.text).unwrap_or_default()
}

pub fn write(s: &str) {
    if active() {
        if let Some(sink) = slot().lock().unwrap().as_mut() {
            sink.text.push_str(s);
            sink.nl += s.matches('\n').count();
            if sink.nl as f64 > MAX_LINES as f64 * 1.2 {
                // keep memory bounded on very long sessions
                let keep: Vec<&str> = sink.text.split('\n').collect();
                sink.text = keep[keep.len() - MAX_LINES..].join("\n");
                sink.nl = sink.text.matches('\n').count();
                sink.generation += 1;
            }
            DIRTY.store(true, Ordering::Relaxed);
            return;
        }
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(s.as_bytes());
    let _ = o.flush();
}

/// Run `f` on the buffer (the screen reads it to draw).
pub fn with<R>(f: impl FnOnce(&Sink) -> R) -> Option<R> {
    slot().lock().unwrap().as_ref().map(f)
}

/// /clear: wipe the transcript.
pub fn clear() {
    if let Some(s) = slot().lock().unwrap().as_mut() {
        s.text.clear();
        s.nl = 0;
        s.generation += 1;
    }
    DIRTY.store(true, Ordering::Relaxed);
}

/// Was anything printed since the last call? (the screen redraws only when something changed)
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}

/// Width the renderers wrap to: the transcript pane in full-screen mode, else the terminal.
pub fn set_width(w: usize) {
    WIDTH.store(w, Ordering::Relaxed);
}

pub fn width_override() -> Option<usize> {
    match WIDTH.load(Ordering::Relaxed) {
        0 => None,
        w => Some(w),
    }
}

#[macro_export]
macro_rules! uprintln {
    () => { $crate::ui::sink::write("\n") };
    ($($a:tt)*) => { $crate::ui::sink::write(&format!("{}\n", format!($($a)*))) };
}

#[macro_export]
macro_rules! uprint {
    ($($a:tt)*) => { $crate::ui::sink::write(&format!($($a)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn buffers_when_installed() {
        install();
        write("a\nb");
        assert_eq!(with(|s| s.text.clone()).unwrap(), "a\nb");
        assert!(take_dirty());
        clear();
        assert_eq!(uninstall(), "");
        assert!(!active());
    }
}
