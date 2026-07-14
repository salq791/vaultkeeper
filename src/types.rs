use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Retention {
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
}

impl Retention {
    /// Parses "daily,weekly,monthly", e.g. "7,4,6".
    pub fn parse_csv(s: &str) -> anyhow::Result<Retention> {
        use anyhow::Context;
        let parts: Vec<u32> = s
            .split(',')
            .map(|p| {
                p.trim()
                    .parse::<u32>()
                    .context("retention must be daily,weekly,monthly numbers")
            })
            .collect::<anyhow::Result<_>>()?;
        anyhow::ensure!(
            parts.len() == 3,
            "retention must have exactly three numbers: daily,weekly,monthly"
        );
        Ok(Retention {
            daily: parts[0],
            weekly: parts[1],
            monthly: parts[2],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_csv_roundtrip_and_rejects() {
        let r = Retention::parse_csv("7,4,6").unwrap();
        assert_eq!((r.daily, r.weekly, r.monthly), (7, 4, 6));
        assert!(Retention::parse_csv("7,4").is_err());
        assert!(Retention::parse_csv("a,b,c").is_err());
    }
}
