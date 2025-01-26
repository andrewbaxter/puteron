use {
    puteron::interface::{
        self,
        demon::Config,
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
    for (k, v) in interface::ipc::ipc::to_json_schema() {
        write(root.join(format!("ipc_{}.schema.json", k)), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }
}
