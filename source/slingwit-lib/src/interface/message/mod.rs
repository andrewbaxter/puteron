use {
    serde::{
        Deserialize,
        Serialize,
    },
};

pub mod v1;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub enum Request {
    V1(v1::Request),
}
