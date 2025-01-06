use {
    crate::time::{
        MinuteSecond,
        SerdeMonth,
        SimpleDuration,
    },
    chrono::{
        NaiveTime,
        Weekday,
    },
    schemars::JsonSchema,
    serde::{
        Deserialize,
        Serialize,
    },
};

#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RulePeriod {
    pub period: SimpleDuration,
    /// Start with a random delay up to the period size, to avoid synchronized restarts
    /// causing thundering herds.
    #[serde(default)]
    pub scattered: bool,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ScheduleHourly {
    // 0-23
    pub minute: usize,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RuleWeekly {
    // Lowercase, English (`monday`, `tuesday`, etc)
    pub weekday: Weekday,
    pub time: NaiveTime,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RuleMonthly {
    // Starting at 1, clamped to month day range
    pub day: usize,
    pub time: NaiveTime,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RuleYearly {
    pub month: SerdeMonth,
    // Starting at 1, clamped to month day range
    pub day: usize,
    pub time: NaiveTime,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Rule {
    Period(RulePeriod),
    Hourly(MinuteSecond),
    Daily(NaiveTime),
    Weekly(RuleWeekly),
    Monthly(RuleMonthly),
    Yearly(RuleYearly),
}
