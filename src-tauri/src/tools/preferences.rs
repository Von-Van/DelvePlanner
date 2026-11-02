//! `get_preferences`: how the person likes to plan, for advice that fits them. Two kinds of
//! knowledge are kept apart: the planning profile the person set themselves (confirmed
//! preferences), and observations Delve Planner computed from their own records (verified
//! patterns, not wishes). Observations the person muted are neither computed nor returned.

use super::error::ToolError;
use super::ToolSources;
use crate::model::{DayPreference, ObservationKind, WorkingHours};
use serde::{Deserialize, Serialize};

const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreferencesArgs {}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub profile: ProfileView,
    pub working_hours: WorkingHours,
    pub observations: Vec<ObservationView>,
}

/// The planning profile as the person set it. Every field is optional; `isSet` is false when the
/// person has said nothing yet, so an empty profile is not mistaken for a preference.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileView {
    pub trust: &'static str,
    pub is_set: bool,
    /// Local "HH:MM" bounds the person prefers to plan work into.
    pub preferred_start: Option<String>,
    pub preferred_end: Option<String>,
    pub max_planned_minutes: Option<i64>,
    pub focus_minutes: Option<i64>,
    pub break_minutes: Option<i64>,
    pub no_work_days: Vec<&'static str>,
    pub energy: Option<DayPreference>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationView {
    pub trust: &'static str,
    pub kind: ObservationKind,
    pub summary: String,
    pub evidence: String,
    pub sample: i64,
}

pub fn get_preferences(
    sources: &ToolSources<'_>,
    _args: PreferencesArgs,
) -> Result<Preferences, ToolError> {
    let profile = sources.database.planning_profile()?;
    let working_hours = sources.database.working_hours()?;
    let observations = sources
        .database
        .observations()?
        .into_iter()
        .map(|observation| ObservationView {
            trust: "verified",
            kind: observation.kind,
            summary: observation.summary,
            evidence: observation.evidence,
            sample: observation.sample,
        })
        .collect();
    let is_set = profile.preferred_start_minute.is_some()
        || profile.preferred_end_minute.is_some()
        || profile.max_planned_minutes.is_some()
        || profile.focus_minutes.is_some()
        || profile.break_minutes.is_some()
        || !profile.no_work_days.is_empty()
        || profile.energy.is_some();
    Ok(Preferences {
        profile: ProfileView {
            trust: "user_confirmed",
            is_set,
            preferred_start: profile.preferred_start_minute.map(clock),
            preferred_end: profile.preferred_end_minute.map(clock),
            max_planned_minutes: profile.max_planned_minutes,
            focus_minutes: profile.focus_minutes,
            break_minutes: profile.break_minutes,
            no_work_days: profile
                .no_work_days
                .iter()
                .filter_map(|day| WEEKDAYS.get(usize::from(*day).wrapping_sub(1)).copied())
                .collect(),
            energy: profile.energy,
        },
        working_hours,
        observations,
    })
}

/// Minutes after local midnight as "HH:MM"; the end of the day is "24:00".
fn clock(minute: i64) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

#[cfg(test)]
mod tests {
    use super::clock;

    #[test]
    fn clock_times_are_zero_padded_and_the_day_ends_at_24() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(9 * 60 + 5), "09:05");
        assert_eq!(clock(1440), "24:00");
    }
}
