//! Plain-text checks for whether a request actually says something: a day, a length, a reminder
//! amount, or a record's name. The resolver uses them to drop values the model invented.

use crate::db::{fold, planning_tokens, title_matches};

const MONTHS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
];

const DAY_WORDS: &[&str] = &[
    "today",
    "tonight",
    "tomorrow",
    "tmrw",
    "yesterday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "mon",
    "tue",
    "tues",
    "wed",
    "thu",
    "thur",
    "thurs",
    "fri",
    "weekend",
];

const BULK_WORDS: &[&str] = &[
    "all",
    "every",
    "everything",
    "each",
    "events",
    "milestones",
    "plans",
    "tasks",
];

const FOLLOW_UP_WORDS: &[&str] = &[
    "it", "that", "them", "those", "also", "too", "actually", "instead", "again", "plus", "well",
];

const AMOUNT_WORDS: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "fifteen",
    "twenty", "thirty", "forty", "fifty", "sixty", "half", "quarter", "hour", "hours", "day",
    "days", "week", "weeks",
];

pub(super) fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric() && character != '-')
        .flat_map(|word| {
            // Keep "to-do" whole, and also look at its parts.
            let parts = word
                .contains('-')
                .then(|| word.split('-').filter(|part| !part.is_empty()))
                .into_iter()
                .flatten();
            std::iter::once(word.to_string()).chain(parts.map(str::to_string))
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Whether `lower` contains `title` as whole words.
pub(super) fn contains_title(lower: &str, title: &str) -> bool {
    let title = fold(title);
    if title.chars().count() < 3 {
        return false;
    }
    lower.match_indices(&title).any(|(index, _)| {
        let before = lower[..index].chars().next_back();
        let after = lower[index + title.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Whether the text names a record by its title or by a distinctive word of it.
pub(super) fn names(text: &str, title: &str) -> bool {
    let lower = text.to_lowercase();
    contains_title(&lower, title) || title_matches(title, &planning_tokens(text)) > 0
}

/// Whether the request hands the choosing to the planner: "when should I do this", "plan my
/// week", "find time for it". Only then may a day or a week the request never gave survive, and
/// only marked as a suggestion. Anything else keeps the old rule that invented dates are dropped.
pub(super) fn asks_to_choose(text: &str) -> bool {
    let words = words(text);
    let has = |phrase: &[&str]| {
        words
            .windows(phrase.len())
            .any(|window| window.iter().zip(phrase).all(|(word, part)| word == part))
    };
    [
        ["when", "should"].as_slice(),
        ["when", "can"].as_slice(),
        ["find", "time"].as_slice(),
        ["fit", "in"].as_slice(),
        ["plan", "my"].as_slice(),
        ["plan", "the"].as_slice(),
        ["plan", "this"].as_slice(),
        ["plan", "out"].as_slice(),
        ["spread", "out"].as_slice(),
        ["help", "me", "plan"].as_slice(),
        ["work", "out", "when"].as_slice(),
        ["you", "pick"].as_slice(),
        ["you", "choose"].as_slice(),
        ["you", "decide"].as_slice(),
    ]
    .iter()
    .any(|phrase| has(phrase))
}

/// Whether the request itself chose a week. "week" alone isn't enough: plan titles such as
/// "Streamer Charity Week" contain it, and a title should never ground a date.
pub(super) fn mentions_week(text: &str) -> bool {
    let words = words(text);
    words.windows(2).any(|pair| {
        pair[1] == "week"
            && matches!(
                pair[0].as_str(),
                "this" | "next" | "following" | "coming" | "same" | "that" | "the" | "a"
            )
    }) || words.iter().any(|word| word == "weekly")
}

pub(super) fn mentions_day(text: &str) -> bool {
    let words = words(text);
    let has_digit_date = text.split_whitespace().any(|word| {
        let word = word.trim_matches(|character: char| !character.is_alphanumeric());
        let digits = word.chars().filter(char::is_ascii_digit).count();
        // 2026-10-03, 10/3, 3rd, 21st
        (digits >= 1 && (word.contains('-') || word.contains('/')))
            || (digits >= 1
                && ["st", "nd", "rd", "th"]
                    .iter()
                    .any(|suffix| word.ends_with(suffix) && word.len() <= 4))
    });
    has_digit_date
        || words.iter().enumerate().any(|(index, word)| {
            DAY_WORDS.contains(&word.as_str())
                || MONTHS.contains(&word.as_str())
                || (word == "may"
                    && words
                        .get(index + 1)
                        .is_some_and(|next| next.chars().all(|c| c.is_ascii_digit())))
                || (word == "in"
                    && words.get(index + 2).is_some_and(|unit| {
                        matches!(unit.as_str(), "day" | "days" | "week" | "weeks")
                    }))
                || (matches!(word.as_str(), "week" | "month")
                    && index
                        .checked_sub(1)
                        .and_then(|previous| words.get(previous))
                        .is_some_and(|previous| {
                            matches!(previous.as_str(), "this" | "next" | "last")
                        }))
        })
}

pub(super) fn mentions_duration(text: &str) -> bool {
    let lower = text.to_lowercase();
    let words = words(text);
    [
        "minute", "hour", "all day", "all-day", "longer", "shorter", "extend", "shorten",
        "lengthen", "duration",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
        || words.iter().any(|word| {
            matches!(word.as_str(), "min" | "mins" | "hr" | "hrs")
                || ((word.ends_with('m') || word.ends_with('h'))
                    && word.len() > 1
                    && word[..word.len() - 1].chars().all(|c| c.is_ascii_digit()))
        })
}

/// Whether the text gives a clock time such as "2 pm", "2:30", "14:00", or "8am".
pub(super) fn mentions_clock_time(text: &str) -> bool {
    let words = text
        .to_lowercase()
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != ':'
            })
            .to_string()
        })
        .collect::<Vec<_>>();
    words.iter().enumerate().any(|(index, word)| {
        let digits = word.trim_end_matches("am").trim_end_matches("pm");
        let clock = !digits.is_empty()
            && digits
                .split(':')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
        clock
            && (word.contains(':')
                || word.ends_with("am")
                || word.ends_with("pm")
                || matches!(
                    words.get(index + 1).map(String::as_str),
                    Some("am" | "pm" | "a.m" | "p.m")
                ))
    })
}

/// Notes are never shown to the model, so it may only write them when the request talks about notes.
pub(super) fn mentions_notes(text: &str) -> bool {
    words(text)
        .iter()
        .any(|word| matches!(word.as_str(), "note" | "notes"))
}

pub(super) fn mentions_reminder(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("remind") || lower.contains("alert me") || lower.contains("notify")
}

/// Whether the text asks for a reminder at the start ("when it starts", "at the start") without
/// stating any offset such as "15 minutes before" or "a 30 minute reminder".
pub(super) fn wants_reminder_at_start(text: &str) -> bool {
    let lower = text.to_lowercase();
    let words = words(text);
    let states_offset = lower.contains("before")
        || words.windows(2).any(|pair| {
            let amount = pair[0].chars().all(|c| c.is_ascii_digit())
                || AMOUNT_WORDS.contains(&pair[0].as_str())
                || matches!(pair[0].as_str(), "a" | "an");
            amount
                && matches!(
                    pair[1].as_str(),
                    "minute"
                        | "minutes"
                        | "min"
                        | "mins"
                        | "hour"
                        | "hours"
                        | "hr"
                        | "hrs"
                        | "day"
                        | "days"
                        | "week"
                        | "weeks"
                )
        });
    mentions_reminder(text) && lower.contains("start") && !states_offset
}

pub(super) fn is_bulk(text: &str) -> bool {
    words(text)
        .iter()
        .any(|word| BULK_WORDS.contains(&word.as_str()))
}

pub(super) fn is_follow_up(text: &str) -> bool {
    words(text)
        .iter()
        .any(|word| FOLLOW_UP_WORDS.contains(&word.as_str()))
}

pub(super) fn mentions_plan_word(text: &str) -> bool {
    words(text)
        .iter()
        .any(|word| matches!(word.as_str(), "plan" | "plans" | "project" | "projects"))
}

/// The user's own spelling of `title` when the request contains it, ignoring case.
pub(super) fn user_spelling(command: &str, title: &str) -> Option<String> {
    if !command.is_ascii() || !title.is_ascii() || title.trim().is_empty() {
        return None;
    }
    let lower_command = command.to_ascii_lowercase();
    let lower_title = title.trim().to_ascii_lowercase();
    lower_command
        .match_indices(&lower_title)
        .find(|(index, _)| {
            let before = lower_command[..*index].chars().next_back();
            let after = lower_command[index + lower_title.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric())
                && !after.is_some_and(|c| c.is_ascii_alphanumeric())
        })
        .map(|(index, _)| command[index..index + lower_title.len()].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_lengths_and_amounts_must_be_stated() {
        for text in [
            "add pick up badges for Thursday",
            "due October 1",
            "on 2027-04-15",
            "by 10/3",
            "on the 3rd",
            "in 3 days",
            "next week",
            "May 4",
        ] {
            assert!(mentions_day(text), "{text}");
        }
        for text in [
            "add call the caterer to my to-do list",
            "it may rain",
            "book venue",
            "add a task to charity week",
        ] {
            assert!(!mentions_day(text), "{text}");
        }
        assert!(mentions_duration("make it 90 minutes"));
        assert!(mentions_duration("add offsite for 24 hours"));
        assert!(mentions_duration("a 45m call"));
        assert!(!mentions_duration("add tea Friday at 10 am"));
        assert!(mentions_clock_time("add dentist at 2 pm"));
        assert!(mentions_clock_time("at 14:30"));
        assert!(mentions_clock_time("breakfast at 8am"));
        assert!(!mentions_clock_time("move release to midnight tomorrow"));
        assert!(!mentions_clock_time("move everything 15 minutes earlier"));
        assert!(wants_reminder_at_start("remind me when planning starts"));
        assert!(wants_reminder_at_start(
            "add tea Friday at 10 am and remind me when it starts"
        ));
        assert!(!wants_reminder_at_start(
            "remind me an hour before it starts"
        ));
        assert!(!wants_reminder_at_start(
            "breakfast with a reminder at start and dinner with a 30 minute reminder"
        ));
    }

    #[test]
    fn user_spelling_keeps_the_request_casing() {
        assert_eq!(
            user_spelling("rename standup to Team standup", "Team Standup").as_deref(),
            Some("Team standup")
        );
        assert_eq!(
            user_spelling("add post-change check today", "Post-Change Check").as_deref(),
            Some("post-change check")
        );
        assert_eq!(
            user_spelling("add dentist tomorrow", "Dentist appointment"),
            None
        );
        assert_eq!(user_spelling("add catering", "Cat"), None);
    }

    #[test]
    fn names_follow_titles_and_distinctive_words() {
        assert!(names("mark email sponsors done", "Email sponsors"));
        assert!(names(
            "put the call in charity week",
            "Streamer Charity Week"
        ));
        assert!(!names("add pick up badges to my tasks", "Home renovation"));
        assert!(is_bulk("mark all my tasks done"));
        assert!(is_follow_up("actually make it due September 30"));
        assert!(!is_follow_up("move gym to 7 pm tomorrow"));
    }
}
