use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use croner::Cron;

pub fn validate(expr: &str) -> Result<()> {
    parse(expr).map(|_| ())
}

pub fn next_occurrence(expr: &str, after: DateTime<Local>) -> Result<DateTime<Local>> {
    parse(expr)?
        .find_next_occurrence(&after, false)
        .with_context(|| format!("no next occurrence for schedule '{expr}'"))
}

fn parse(expr: &str) -> Result<Cron> {
    Cron::new(expr)
        .parse()
        .with_context(|| format!("invalid cron schedule '{expr}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn valid_five_field_cron_accepted() {
        assert!(validate("0 2 * * *").is_ok());
        assert!(validate("*/15 * * * *").is_ok());
    }

    #[test]
    fn garbage_rejected_naming_the_expression() {
        let err = validate("not a cron").unwrap_err();
        assert!(err.to_string().contains("not a cron"));
    }

    #[test]
    fn next_occurrence_advances_to_the_scheduled_time() {
        let after = chrono::Local
            .with_ymd_and_hms(2026, 1, 1, 0, 30, 0)
            .unwrap();
        let next = next_occurrence("0 2 * * *", after).unwrap();
        assert_eq!(
            next,
            chrono::Local.with_ymd_and_hms(2026, 1, 1, 2, 0, 0).unwrap()
        );
    }
}
