//! Composition-root lifecycle hooks for the agent ([`nerve_agent::Hook`]).
//!
//! Seam discipline: the [`Hook`] trait is the observe/augment seam *owned by*
//! `nerve-agent`; this module is the binary's concrete *use* of it. Wall-clock
//! access (today's date) lives here — never in the deterministic kernel — and is
//! read at construction so the hook itself stays pure and unit-testable.

use nerve_agent::Hook;
use std::path::PathBuf;

/// Built-in hook that grounds the agent in its environment: it appends today's
/// date and the working root to the system prompt before the run starts. Both
/// values are captured at construction, so the hook is deterministic; the only
/// non-determinism (reading the clock) stays at the call site.
pub(crate) struct EnvironmentHook {
    date: String,
    root: Option<PathBuf>,
}

impl EnvironmentHook {
    pub(crate) fn new(date: String, root: Option<PathBuf>) -> Self {
        Self { date, root }
    }
}

impl Hook for EnvironmentHook {
    fn on_start(&self, system_prompt: &mut String) {
        system_prompt.push_str("\n\nEnvironment:\n- Today's date is ");
        system_prompt.push_str(&self.date);
        system_prompt.push('.');
        if let Some(root) = &self.root {
            system_prompt.push_str("\n- Working root: ");
            system_prompt.push_str(&root.display().to_string());
        }
    }
}

/// Current UTC date as `YYYY-MM-DD`. Reads the wall clock (a composition-root
/// concern); falls back to the epoch date if the clock predates 1970.
pub(crate) fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    civil_date_utc(secs)
}

/// Convert a UNIX timestamp (seconds) into a `YYYY-MM-DD` UTC date using Howard
/// Hinnant's `civil_from_days` algorithm — pure and dependency-free, hence
/// unit-testable against fixed timestamps.
fn civil_date_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    // Shift the epoch to 0000-03-01 so leap days fall at the end of each cycle.
    // `secs` is unsigned, so `z` is always non-negative and Hinnant's negative-day
    // era adjustment is unnecessary.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097; // day-of-era, [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year (Mar 1 = 0)
    let mp = (5 * doy + 2) / 153; // month shifted so Mar = 0, [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_matches_known_timestamps() {
        assert_eq!(civil_date_utc(0), "1970-01-01");
        assert_eq!(civil_date_utc(86_400), "1970-01-02");
        // 2024-01-01T00:00:00Z
        assert_eq!(civil_date_utc(1_704_067_200), "2024-01-01");
        // 2000-02-29T00:00:00Z — exercises a leap day.
        assert_eq!(civil_date_utc(951_782_400), "2000-02-29");
    }

    #[test]
    fn environment_hook_appends_date_and_root() {
        let hook =
            EnvironmentHook::new("2026-06-18".to_string(), Some(PathBuf::from("/work/proj")));
        let mut prompt = "base prompt".to_string();
        hook.on_start(&mut prompt);
        // The original prompt is preserved and the environment section appended.
        assert!(prompt.starts_with("base prompt"));
        assert!(prompt.contains("Today's date is 2026-06-18."));
        assert!(prompt.contains("Working root: /work/proj"));
    }

    #[test]
    fn environment_hook_omits_absent_root() {
        let hook = EnvironmentHook::new("2026-06-18".to_string(), None);
        let mut prompt = String::new();
        hook.on_start(&mut prompt);
        assert!(prompt.contains("Today's date is 2026-06-18."));
        assert!(!prompt.contains("Working root"));
    }
}
