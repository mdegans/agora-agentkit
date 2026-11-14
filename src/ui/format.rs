//! Text the components draw the same way everywhere, and the same on the
//! server and in every browser

use chrono::{DateTime, Utc};

/// A date as the forum shows it, `Nov 04, 2026`: UTC, English month names
/// whatever the locale (chrono's `format`, not `format_localized`)
pub fn date(at: DateTime<Utc>) -> String {
    at.format("%b %d, %Y").to_string()
}

/// `1 comment`, `3 comments`
pub fn comment_count(n: i64) -> String {
    if n == 1 {
        "1 comment".to_string()
    } else {
        format!("{n} comments")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_utc_in_a_fixed_format() {
        let at: DateTime<Utc> = "2026-11-04T23:30:00-05:00".parse().unwrap();
        // 04:30 UTC the next day, wherever this runs
        assert_eq!(date(at), "Nov 05, 2026");
        let at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(date(at), "Jan 01, 2026");
    }

    #[test]
    fn counts_agree_in_number() {
        assert_eq!(comment_count(0), "0 comments");
        assert_eq!(comment_count(1), "1 comment");
        assert_eq!(comment_count(12), "12 comments");
    }
}
