use serde::{Deserialize, Serialize};

/// Risk level of a ticket or a device action.
/// L1 = automatic, L2 = team approver, L3 = owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Level(u8);

impl Level {
    pub const L1: Level = Level(1);
    pub const L2: Level = Level(2);
    pub const L3: Level = Level(3);

    /// The final level: deterministic rules give the floor, a model may
    /// suggest a level, and the result is never below the floor.
    pub fn with_model_suggestion(floor: Level, suggestion: Level) -> Level {
        floor.max(suggestion)
    }

    pub fn needs_human(self) -> bool {
        self >= Level::L2
    }
}

impl TryFrom<u8> for Level {
    type Error = String;
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            1..=3 => Ok(Level(v)),
            _ => Err(format!("level must be 1, 2 or 3, got {v}")),
        }
    }
}

impl From<Level> for u8 {
    fn from(l: Level) -> u8 {
        l.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_can_raise_but_never_lower() {
        assert_eq!(
            Level::with_model_suggestion(Level::L2, Level::L1),
            Level::L2
        );
        assert_eq!(
            Level::with_model_suggestion(Level::L1, Level::L3),
            Level::L3
        );
        assert_eq!(
            Level::with_model_suggestion(Level::L3, Level::L1),
            Level::L3
        );
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(Level::try_from(0).is_err());
        assert!(Level::try_from(4).is_err());
        assert!(Level::try_from(2).is_ok());
    }

    #[test]
    fn only_l2_and_up_need_a_human() {
        assert!(!Level::L1.needs_human());
        assert!(Level::L2.needs_human());
        assert!(Level::L3.needs_human());
    }
}
