use {
    schemars::JsonSchema,
    serde::{
        Deserialize,
        Serialize,
    },
};

pub mod v1;

pub use v1 as latest;

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub enum Request {
    V1(v1::Request),
}
