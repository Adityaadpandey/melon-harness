//! How well the agent guesses its own effort. Before a request it names how many tool rounds it needs; afterwards the
//! guess is scored against what really happened and written to a small file. The next guess is made with the last few
//! results in front of it, so it learns from its own record (there are no weights to train, only this feedback).
//!
//! Example: guessed 10, used 8 -> +10. Guessed 10, used 15 -> -5. Hit the hard stop -> -25.
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The hard stop sits at three times the guess, never below this: it catches a runaway, not an ordinary long task.
const FLOOR: usize = 25;
const RECENT: usize = 10;

pub fn backstop(estimate: usize) -> usize {
    (estimate * 3).max(FLOOR)
}

pub fn file(dir: &Path) -> PathBuf {
    dir.join("calibration.jsonl")
}

/// Points for one finished request.
/// - at or under the guess: +10 (+5 when the guess was padded to more than 2.5x what was needed)
/// - over: minus one point per tenth over, at most 15
/// - cut off by the hard stop, the clock or a failure: -25, the worst result, because the job is not done
pub fn points(estimate: usize, actual: usize, cut: bool) -> i32 {
    if cut {
        return -25;
    }
    if actual <= estimate {
        return if actual * 5 < estimate * 2 { 5 } else { 10 };
    }
    let over = (actual - estimate) as f64 / estimate.max(1) as f64;
    -((over * 10.0).ceil() as i32).clamp(1, 15)
}

/// One line the user sees when the request ends.
pub fn verdict(estimate: usize, actual: usize, cut: bool, pts: i32) -> String {
    let how = if cut {
        "stopped before the job was done".to_string()
    } else if actual <= estimate {
        "within the target".to_string()
    } else {
        format!("{} over", actual - estimate)
    };
    format!("Rounds: {actual} used, {estimate} estimated, {pts:+} points ({how})")
}

/// Appends the result and returns the points. A file that cannot be written only loses the record.
pub fn record(dir: &Path, intent: &str, estimate: usize, actual: usize, cut: bool) -> i32 {
    let pts = points(estimate, actual, cut);
    let line = json!({"intent": intent, "estimate": estimate, "actual": actual, "cut": cut, "points": pts});
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file(dir)) {
        let _ = writeln!(f, "{line}");
    }
    pts
}

/// The last results as a short paragraph for the estimating prompt, or "" before there is any history.
pub fn track_record(dir: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(file(dir)) else { return String::new() };
    let rows: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let rows = &rows[rows.len().saturating_sub(RECENT)..];
    if rows.is_empty() {
        return String::new();
    }
    let n = |r: &Value, k: &str| r[k].as_u64().unwrap_or(0) as usize;
    let cut = rows.iter().filter(|r| r["cut"] == true).count();
    let over: Vec<f64> = rows.iter().filter(|r| r["cut"] != true && n(r, "actual") > n(r, "estimate")).map(|r| n(r, "actual") as f64 / n(r, "estimate").max(1) as f64).collect();
    let hit = rows.len() - cut - over.len();
    let recent = rows.iter().rev().take(5).map(|r| format!("{} {}->{}", r["intent"].as_str().unwrap_or("?"), n(r, "estimate"), n(r, "actual"))).collect::<Vec<_>>().join(", ");
    let mut out = format!("YOUR TRACK RECORD (last {}): {hit} finished within the estimate, {} ran over, {cut} were stopped. Recent as estimate->used: {recent}.", rows.len(), over.len());
    if !over.is_empty() {
        let avg = over.iter().sum::<f64>() / over.len() as f64;
        out.push_str(&format!(" When you ran over it was by {avg:.1}x on average: lean higher."));
    } else if cut == 0 && hit == rows.len() {
        out.push_str(" Well judged, keep it up; do not pad the number to play safe either.");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_rewards_a_close_guess_and_punishes_overshoot_and_cutoffs() {
        assert_eq!(points(10, 8, false), 10);
        assert_eq!(points(10, 10, false), 10);
        assert_eq!(points(10, 3, false), 5); // padded
        assert_eq!(points(10, 12, false), -2);
        assert_eq!(points(10, 100, false), -15);
        assert_eq!(points(10, 2, true), -25); // cut off beats every other result for worst
        assert_eq!((backstop(4), backstop(20)), (25, 60));
    }

    #[test]
    fn the_record_is_kept_and_fed_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(track_record(dir.path()), "");
        record(dir.path(), "change", 10, 8, false);
        assert!(track_record(dir.path()).contains("Well judged"));
        record(dir.path(), "explain", 4, 8, false);
        record(dir.path(), "change", 10, 30, true);
        let t = track_record(dir.path());
        assert!(t.contains("1 finished within") && t.contains("1 ran over") && t.contains("1 were stopped"), "{t}");
        assert!(t.contains("explain 4->8") && t.contains("lean higher"), "{t}");
    }
}
