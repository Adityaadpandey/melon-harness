//! Which parts of the full-screen layout are showing. `/layout` lists them, `/layout read off` hides one, F3 flips the
//! whole side panel. The choice is kept in the data folder, so it is the same next time.
//! Hiding a part only hides it: the work still happens (a hidden READ list still tracks reads, and quiet mode prints
//! those reads in the conversation instead).
use std::sync::Mutex;

/// (key, what it is). The first four are side-panel sections; `side` hides the panel as a whole.
pub const PARTS: &[(&str, &str)] = &[
    ("side", "the whole right-hand panel"),
    ("agents", "side panel: sub-agents running and finished"),
    ("read", "side panel: files zerd has read"),
    ("written", "side panel: files it changed, with +/- counts"),
    ("commands", "side panel: shell commands it ran"),
    ("hints", "the shortcut hint at the right end of the top bar"),
];

static HIDDEN: Mutex<Option<Vec<String>>> = Mutex::new(None);

fn file() -> std::path::PathBuf {
    crate::config::loader::get_data_dir().join("layout")
}

fn with<T>(f: impl FnOnce(&mut Vec<String>) -> T) -> T {
    let mut g = HIDDEN.lock().unwrap();
    let v = g.get_or_insert_with(|| std::fs::read_to_string(file()).map(|t| t.split_whitespace().map(String::from).collect()).unwrap_or_default());
    f(v)
}

pub fn shown(part: &str) -> bool {
    with(|v| !v.iter().any(|p| p == part))
}

/// Show or hide a part (`None` flips it). Returns the new state, or None for a name that isn't a part.
pub fn set(part: &str, on: Option<bool>) -> Option<bool> {
    PARTS.iter().find(|p| p.0 == part)?;
    let now = on.unwrap_or(!shown(part));
    with(|v| {
        v.retain(|p| p != part);
        if !now {
            v.push(part.to_string());
        }
        let _ = std::fs::create_dir_all(file().parent().unwrap());
        let _ = std::fs::write(file(), v.join("\n"));
    });
    Some(now)
}

pub fn reset() {
    with(|v| v.clear());
    let _ = std::fs::remove_file(file());
}

/// The /layout command: `/layout`, `/layout read`, `/layout read off`, `/layout reset`. Returns the text to print.
pub fn command(args: &str) -> String {
    let w: Vec<&str> = args.split_whitespace().collect();
    match w.as_slice() {
        [] => {
            let mut s = String::from("Layout (full-screen mode). /layout <part> flips one, /layout <part> on|off sets it, /layout reset restores all.\n");
            for (k, d) in PARTS {
                s += &format!("  {} {k:<9} {d}\n", if shown(k) { "on " } else { "off" });
            }
            s + "  F3 flips the side panel from the keyboard."
        }
        ["reset"] => {
            reset();
            "Layout reset: everything is showing.".into()
        }
        [part, rest @ ..] => {
            let on = match rest.first().copied() {
                None => None,
                Some("on") => Some(true),
                Some("off") => Some(false),
                Some(x) => return format!("Use on or off, not {x:?}."),
            };
            match set(part, on) {
                Some(n) => format!("{part}: {}", if n { "on" } else { "off" }),
                None => format!("No layout part called {part:?}. Run /layout to see them."),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_hide_show_persist_and_reset() {
        let d = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZERD_DATA_DIR", d.path()) };
        reset();
        assert!(shown("read") && shown("side"));
        assert_eq!(command("read off"), "read: off");
        assert!(!shown("read") && shown("written"));
        assert_eq!(std::fs::read_to_string(d.path().join("layout")).unwrap(), "read");
        assert_eq!(command("read"), "read: on"); // no argument flips it
        assert!(command("sidebar").contains("No layout part"));
        command("side off");
        command("reset");
        assert!(shown("side"));
    }
}
