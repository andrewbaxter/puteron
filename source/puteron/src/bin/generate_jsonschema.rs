use {
    puteron_lib::interface::{
        self,
        demon::Config,
        message::v1::{
            RequestAdd,
            RequestDemonEnv,
            RequestDemonListSchedule,
            RequestDemonSpecDirs,
            RequestTaskDelete,
            RequestTaskGetSpec,
            RequestTaskGetStatus,
            RequestTaskListDownstream,
            RequestTaskListUpstream,
            RequestTaskListUserOn,
            RequestTaskOn,
            RequestTaskWaitStarted,
            RequestTaskWaitStopped,
        },
        task::Task,
    },
    schemars::schema_for,
    std::{
        env,
        fs::{
            create_dir_all,
            write,
        },
        path::PathBuf,
    },
};

fn main() {
    let root = PathBuf::from(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../generated/jsonschema");
    create_dir_all(&root).unwrap();
    write(root.join("task.schema.json"), serde_json::to_vec_pretty(&schema_for!(Task)).unwrap()).unwrap();
    write(root.join("config.schema.json"), serde_json::to_vec_pretty(&schema_for!(Config)).unwrap()).unwrap();
    write(
        root.join("api_request.schema.json"),
        serde_json::to_vec_pretty(&schema_for!(interface::message::Request)).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_add.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestAdd as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_on.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestTaskOn as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_delete.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestTaskDelete as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_get_status.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestTaskGetStatus as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_get_spec.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestTaskGetSpec as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_wait_started.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestTaskWaitStarted as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_wait_stopped.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestTaskWaitStopped as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_list_user_on.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestTaskListUserOn as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_list_upstream.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestTaskListUpstream as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_task_list_downstream.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestTaskListDownstream as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_demon_env.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestDemonEnv as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_demon_list_schedule.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestDemonListSchedule as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
    write(
        root.join("api_response_v1_demon_spec_dirs.schema.json"),
        serde_json::to_vec_pretty(
            &schema_for!(<RequestDemonSpecDirs as interface::message::v1::RequestTrait>::Response),
        ).unwrap(),
    ).unwrap();
}
