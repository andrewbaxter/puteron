use {
    serde::{
        Deserialize,
        Serialize,
    },
    structre::structre,
};

#[derive(Clone, Copy)]
pub enum SimpleDurationUnit {
    Second,
    Minute,
    Hour,
}

#[derive(Clone, Copy)]
pub struct SimpleDuration {
    pub count: usize,
    pub unit: SimpleDurationUnit,
}

pub const SUFFIX_SECOND: &str = "s";
pub const SUFFIX_MINUTE: &str = "m";
pub const SUFFIX_HOUR: &str = "h";

impl Serialize for SimpleDuration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer {
        return format_args!("{}{}", self.count, match self.unit {
            SimpleDurationUnit::Second => SUFFIX_SECOND,
            SimpleDurationUnit::Minute => SUFFIX_MINUTE,
            SimpleDurationUnit::Hour => SUFFIX_HOUR,
        }).serialize(serializer);
    }
}

impl<'de> Deserialize<'de> for SimpleDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de> {
        #[structre("(?<count>\\d+)(?<unit>[a-z]+)")]
        struct Parser<'a> {
            count: usize,
            unit: &'a str,
        }

        let s = <&str>::deserialize(deserializer)?;
        let p = Parser::try_from(s).map_err(|e| serde::de::Error::custom(e))?;
        return Ok(Self {
            count: p.count,
            unit: match p.unit {
                SUFFIX_SECOND => SimpleDurationUnit::Second,
                SUFFIX_MINUTE => SimpleDurationUnit::Minute,
                SUFFIX_HOUR => SimpleDurationUnit::Hour,
                s => return Err(serde::de::Error::custom(format!("Unknown time unit suffix [{}]", s))),
            },
        });
    }
}
