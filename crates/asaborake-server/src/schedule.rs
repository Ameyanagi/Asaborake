//! When the queue is allowed to work.
//!
//! A recording box is usually somebody's home server, and an encode saturates
//! a GPU and most of a CPU for an hour. Amatsukaze has an hour-by-hour
//! schedule so that work happens overnight and the machine is quiet while
//! anybody is using it — or watching television on it, which is the same
//! machine.
//!
//! Deliberately hours rather than a cron expression. The question being
//! answered is "may I start something now", asked every couple of seconds, and
//! an hour is the finest granularity that question deserves.

use chrono::{Local, Timelike};
use serde::{Deserialize, Serialize};

/// The hours during which jobs may run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunHours {
    /// Hours of the day, 0 to 23. Empty means no restriction.
    hours: Vec<u32>,
}

impl RunHours {
    /// A schedule allowing the given hours.
    #[must_use]
    pub fn new(hours: impl IntoIterator<Item = u32>) -> Self {
        let mut hours: Vec<u32> = hours.into_iter().filter(|h| *h < 24).collect();
        hours.sort_unstable();
        hours.dedup();
        Self { hours }
    }

    /// Whether any restriction applies at all.
    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        self.hours.is_empty()
    }

    /// Whether work may start at `hour`.
    #[must_use]
    pub fn allows_hour(&self, hour: u32) -> bool {
        self.is_unrestricted() || self.hours.contains(&hour)
    }

    /// Whether work may start now, by the machine's own clock.
    ///
    /// Local time rather than UTC: somebody setting "01 to 06" means the small
    /// hours where they live, not in Greenwich.
    #[must_use]
    pub fn allows_now(&self) -> bool {
        self.allows_hour(Local::now().hour())
    }

    /// The hours, in order, for reporting.
    #[must_use]
    pub fn hours(&self) -> &[u32] {
        &self.hours
    }

    /// A description an operator can check against what they meant.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.is_unrestricted() {
            return "any time".to_owned();
        }
        // Runs rather than a list of twenty numbers: "01:00-07:00" is what
        // somebody meant, and what they can check at a glance.
        let mut spans: Vec<String> = Vec::new();
        let mut start = self.hours[0];
        let mut previous = start;
        for &hour in &self.hours[1..] {
            if hour != previous + 1 {
                spans.push(format!("{start:02}:00-{:02}:00", previous + 1));
                start = hour;
            }
            previous = hour;
        }
        spans.push(format!("{start:02}:00-{:02}:00", previous + 1));
        spans.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hours_means_no_restriction() {
        let schedule = RunHours::default();
        assert!(schedule.is_unrestricted());
        for hour in 0..24 {
            assert!(schedule.allows_hour(hour), "hour {hour} was refused");
        }
        assert_eq!(schedule.describe(), "any time");
    }

    #[test]
    fn only_the_listed_hours_are_allowed() {
        let schedule = RunHours::new([1, 2, 3, 4, 5]);
        assert!(!schedule.allows_hour(0));
        assert!(schedule.allows_hour(3));
        assert!(!schedule.allows_hour(6));
        assert!(!schedule.allows_hour(23));
    }

    #[test]
    fn a_schedule_reads_back_as_the_spans_somebody_meant() {
        // Twenty numbers is not something anybody can check at a glance.
        assert_eq!(RunHours::new([1, 2, 3, 4, 5, 6]).describe(), "01:00-07:00");
        assert_eq!(
            RunHours::new([0, 1, 2, 22, 23]).describe(),
            "00:00-03:00, 22:00-24:00"
        );
        assert_eq!(RunHours::new([13]).describe(), "13:00-14:00");
    }

    #[test]
    fn a_schedule_is_tidied_rather_than_taken_literally() {
        // It arrives from a hand-edited configuration file.
        let schedule = RunHours::new([5, 1, 5, 99, 3]);
        // Sorted, de-duplicated, and the impossible hour dropped rather than
        // wrapped into a real one it was never meant to name.
        assert_eq!(schedule.hours(), [1, 3, 5]);
        assert!(!schedule.allows_hour(99));
    }

    #[test]
    fn an_overnight_span_wrapping_midnight_is_two_runs() {
        // 22:00 to 04:00 is written as the hours it covers, and reads back as
        // the two spans it is, because the day boundary is real.
        let schedule = RunHours::new([22, 23, 0, 1, 2, 3]);
        assert!(schedule.allows_hour(23));
        assert!(schedule.allows_hour(0));
        assert!(!schedule.allows_hour(4));
        assert_eq!(schedule.describe(), "00:00-04:00, 22:00-24:00");
    }
}
