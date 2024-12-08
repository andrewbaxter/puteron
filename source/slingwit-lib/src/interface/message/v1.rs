use {
    crate::interface::base::TaskId,
    serde::{
        Deserialize,
        Serialize,
    },
};

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct RequestTaskOn {
    pub task: TaskId,
    pub on: bool,
}

pub type ResponseTaskOn = ();

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub enum Request {
    TaskOn(RequestTaskOn),
}
