use {
    loga::{
        ea,
        ErrContext,
        ResultContext,
    },
    puteron_lib::interface,
    std::{
        collections::{
            BTreeMap,
            HashMap,
        },
        fs::read_dir,
        io::ErrorKind,
        path::PathBuf,
    },
    tracing::debug,
};

pub(crate) fn merge_specs(
    dirs: &[PathBuf],
    filter: Option<&str>,
) -> Result<BTreeMap<String, interface::task::Task>, loga::Error> {
    let mut task_json = HashMap::new();
    for dir in dirs {
        let dir_entries = match read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                if e.kind() == ErrorKind::NotFound {
                    debug!(dir =? dir, "Task directory doesn't exist, skipping");
                    continue;
                }
                return Err(
                    e.context_with("Unable to read specified task directory", ea!(dir = dir.to_string_lossy())),
                );
            },
        };
        let mut dir_entries1 = vec![];
        for e in dir_entries {
            let e = e.context("Error reading task directory entry")?;
            dir_entries1.push(e);
        }
        dir_entries1.sort_by_cached_key(|k| k.file_name());
        let dir_entries = dir_entries1;
        for e in dir_entries {
            let path = e.path();
            let task_name =
                String::from_utf8(
                    path.file_stem().unwrap().as_encoded_bytes().to_vec(),
                ).context_with("Task directory entry has invalid unicode name", ea!(path = path.to_string_lossy()))?;
            if let Some(filter) = filter {
                if task_name != filter {
                    continue;
                }
            }
            let mut config =
                serde_json::from_slice::<serde_json::Value>(
                    &std::fs::read(
                        &path,
                    ).context_with("Error reading json from task directory", ea!(path = path.to_string_lossy()))?,
                ).context_with("Task definition has invalid json", ea!(path = path.to_string_lossy()))?;
            if let Some(lower) = task_json.remove(&task_name) {
                fn merge(lower: serde_json::Value, upper: serde_json::Value) -> serde_json::Value {
                    match (lower, upper) {
                        (serde_json::Value::Object(mut lower), serde_json::Value::Object(upper)) => {
                            for (k, mut upper_child) in upper {
                                if let Some(lower_child) = lower.remove(&k) {
                                    upper_child = merge(lower_child, upper_child);
                                }
                                lower.insert(k, upper_child);
                            }
                            return serde_json::Value::Object(lower);
                        },
                        (_, upper) => {
                            return upper;
                        },
                    }
                }

                config = merge(lower, config);
            }
            task_json.insert(task_name, config);
        }
    }
    let mut tasks = BTreeMap::new();
    for (id, value) in task_json {
        let config =
            serde_json::from_value::<interface::task::Task>(
                value.clone(),
            ).context_with(
                "Task has invalid definition",
                ea!(id = id, config = serde_json::to_string_pretty(&value).unwrap()),
            )?;
        tasks.insert(id, config);
    }
    return Ok(tasks);
}
