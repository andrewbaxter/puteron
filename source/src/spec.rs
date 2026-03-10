use {
    crate::{
        errors::ErrorHandler,
        interface::{
            self,
            base::TaskId,
            task::Task,
        },
    },
    flowcontrol::ta_return,
    loga::{
        DebugDisplay,
        ErrContext,
        ResultContext,
        ea,
    },
    sha2::{
        Digest,
        Sha256,
    },
    std::{
        collections::{
            BTreeMap,
            HashMap,
            HashSet,
        },
        path::PathBuf,
    },
    tokio::fs::read_dir,
};

pub async fn list_task_dir_tasks(dirs: &[PathBuf], errors: &mut dyn ErrorHandler) -> HashMap<TaskId, Vec<PathBuf>> {
    let mut out = HashMap::<TaskId, Vec<PathBuf>>::new();
    for dir in dirs {
        let mut dir_entries = match read_dir(&dir).await {
            Ok(e) => e,
            Err(e) => {
                errors.handle(
                    e.context_with("Unable to read specified task directory", ea!(dir = dir.to_string_lossy())),
                );
                continue;
            },
        };
        let mut dir_entries1 = vec![];
        loop {
            let e = match dir_entries.next_entry().await {
                Ok(e) => e,
                Err(e) => {
                    errors.handle(e.context("Error reading task directory entry"));
                    continue;
                },
            };
            let Some(e) = e else {
                break;
            };
            dir_entries1.push(e);
        }
        dir_entries1.sort_by_cached_key(|k| k.file_name());
        let dir_entries = dir_entries1;
        for e in dir_entries {
            let path = e.path();
            if let Some(ext) = path.extension() {
                if ext.as_encoded_bytes() != b"json" {
                    continue;
                }
            }
            let task_id = match String::from_utf8(path.file_stem().unwrap().as_encoded_bytes().to_vec()) {
                Ok(v) => v,
                Err(e) => {
                    errors.handle(
                        e.context_with(
                            "Task directory entry has invalid unicode name",
                            ea!(path = path.to_string_lossy()),
                        ),
                    );
                    continue;
                },
            };
            out.entry(task_id.clone()).or_default().push(path);
        }
    }
    return out;
}

pub async fn merge_specs(
    fs_tasks: HashMap<TaskId, Vec<PathBuf>>,
    errors: &mut dyn ErrorHandler,
) -> BTreeMap<String, (HashMap<PathBuf, Vec<u8>>, interface::task::Task)> {
    let mut tasks = BTreeMap::new();
    for (task_name, paths) in fs_tasks {
        let mut value = None;
        let mut hashes = HashMap::new();
        for path in paths {
            match async {
                ta_return!((), loga::Error);
                let bytes =
                    std::fs::read(
                        &path,
                    ).context_with("Error reading json from task directory", ea!(path = path.to_string_lossy()))?;
                let hash = Sha256::digest(&bytes).to_vec();
                hashes.insert(path.clone(), hash);
                let upper =
                    serde_json::from_slice::<serde_json::Value>(
                        &bytes,
                    ).context_with("Task definition has invalid json", ea!(path = path.to_string_lossy()))?;
                if let Some(lower) = value.take() {
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

                    value = Some(merge(lower, upper));
                } else {
                    value = Some(upper);
                }
                return Ok(());
            }.await {
                Ok(_) => { },
                Err(e) => {
                    errors.handle(e);
                },
            }
        }
        let value = value.unwrap();
        let task =
            match serde_path_to_error::deserialize::<_, interface::task::Task>(
                &mut serde_json::Deserializer::from_slice(
                    // https://github.com/serde-rs/json/issues/1233
                    &serde_json::to_vec(&value).unwrap(),
                ),
            ) {
                Ok(v) => v,
                Err(e) => {
                    errors.handle(
                        e.context_with(
                            "Task has invalid definition",
                            ea!(id = task_name, config = serde_json::to_string_pretty(&value).unwrap()),
                        ),
                    );
                    continue;
                },
            };
        tasks.insert(task_name.clone(), (hashes, task));
    }
    return tasks;
}

pub fn order_specs(
    mut built_specs: HashSet<TaskId>,
    errors: &mut dyn ErrorHandler,
    mut new_specs: BTreeMap<String, (HashMap<PathBuf, Vec<u8>>, Task)>,
) -> (Vec<(TaskId, Task)>, HashMap<TaskId, HashMap<PathBuf, Vec<u8>>>) {
    let mut new_hashes = HashMap::new();
    let mut out = vec![];
    let mut missing_upstreams = HashSet::new();
    while !new_specs.is_empty() {
        let mut did_work = false;
        let task_ids = new_specs.keys().cloned().collect::<Vec<_>>();
        for task_id in &task_ids {
            // Find frontier tasks (all upstreams created)
            let (_, spec) = new_specs.get(task_id).unwrap();
            let upstream: Vec<&String> = match &spec {
                Task::Empty(s) => {
                    s.upstream.keys().collect()
                },
                Task::Long(s) => {
                    s.upstream.keys().collect()
                },
                Task::Short(s) => {
                    s.upstream.keys().collect()
                },
            };
            let mut all_upstream_created = true;
            for upstream_id in upstream {
                if built_specs.contains(upstream_id) {
                    // created, ok
                } else if new_specs.contains_key(upstream_id) {
                    // not yet created
                    all_upstream_created = false;
                } else {
                    // missing, pretend ok - missing will be logged later when validating
                    all_upstream_created = false;
                    missing_upstreams.insert((task_id.clone(), upstream_id.clone()));
                }
            }
            if !all_upstream_created {
                continue;
            }

            // All deps created, now create this task
            did_work = true;
            built_specs.insert(task_id.clone());
            let (hashes, task) = new_specs.remove(task_id).unwrap();
            new_hashes.insert(task_id.clone(), hashes);
            out.push((task_id.clone(), task));
        }
        if !did_work {
            if !missing_upstreams.is_empty() {
                errors.handle(
                    loga::err_with(
                        "One or more tasks have invalid upstreams",
                        ea!(
                            missing =
                                missing_upstreams
                                    .iter()
                                    .map(|(k, v)| format!("upstream [{}] (from [{}])", v, k))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                        ),
                    ),
                );
            } else {
                errors.handle(
                    loga::err_with(
                        "One or more tasks have cycles in their dependencies or invalid upstreams",
                        ea!(tasks = task_ids.dbg_str()),
                    ),
                );
            }
            break;
        }
    }
    return (out, new_hashes);
}
