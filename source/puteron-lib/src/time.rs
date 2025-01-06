use {
    chrono::Month,
    schemars::{
        schema::{
            InstanceType,
            SchemaObject,
            StringValidation,
        },
        JsonSchema,
    },
    serde::{
        Deserialize,
        Serialize,
    },
    std::{
        borrow::Cow,
        str::FromStr,
        time::Duration,
    },
    structre::structre,
};

// # Fixed duration
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SimpleDurationUnit {
    Second,
    Minute,
    Hour,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SimpleDuration {
    pub count: u64,
    pub unit: SimpleDurationUnit,
}

impl Into<Duration> for SimpleDuration {
    fn into(self) -> Duration {
        match self.unit {
            SimpleDurationUnit::Second => return Duration::from_secs(self.count),
            SimpleDurationUnit::Minute => return Duration::from_secs(self.count * 60),
            SimpleDurationUnit::Hour => return Duration::from_secs(self.count * 60 * 60),
        }
    }
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
        #[structre("(?<count>[0-9]+)(?<unit>[a-z]+)")]
        struct Parser<'a> {
            count: u64,
            unit: &'a str,
        }

        let s = <Cow<str>>::deserialize(deserializer)?;
        let p = Parser::try_from(s.as_ref()).map_err(|e| serde::de::Error::custom(e))?;
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

impl JsonSchema for SimpleDuration {
    fn schema_name() -> String {
        return "Duration".to_string();
    }

    fn json_schema(_gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        return SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                pattern: Some("(\\[0-9]+)([hms])".to_string()),
                ..Default::default()
            })),
            ..Default::default()
        }.into();
    }
}

// # Calendar relative
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MinuteSecond {
    pub minute: u8,
    pub second: u8,
}

impl JsonSchema for MinuteSecond {
    fn schema_name() -> String {
        return "MinuteSecond".to_string();
    }

    fn json_schema(_gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        return SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                pattern: Some("[0-5]?[0-9](:[0-5][0-9])?".to_string()),
                ..Default::default()
            })),
            ..Default::default()
        }.into();
    }
}

impl Serialize for MinuteSecond {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer {
        return format_args!("{}:{}", self.minute, self.second).serialize(serializer);
    }
}

impl<'de> Deserialize<'de> for MinuteSecond {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de> {
        #[structre("(?<minute>[0-5]?[0-9])(?<second>(:[0-5][0-9])?)")]
        struct Parser {
            minute: u8,
            second: u8,
        }

        let parsed = Parser::from_str(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)?;
        return Ok(Self {
            minute: parsed.minute,
            second: parsed.second,
        });
    }
}

// # Month with jsonschema
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct SerdeMonth(pub Month);

impl JsonSchema for SerdeMonth {
    fn schema_name() -> String {
        return "Month".to_string();
    }

    fn json_schema(_gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        return SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            format: Some("january|february|march|...".to_string()),
            ..Default::default()
        }.into();
    }
}

impl Serialize for SerdeMonth {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer {
        return Month::serialize(&self.0, serializer);
    }
}

impl<'de> Deserialize<'de> for SerdeMonth {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de> {
        return Ok(SerdeMonth(Month::deserialize(deserializer)?));
    }
}
