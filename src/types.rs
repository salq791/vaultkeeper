use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Retention {
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
}
