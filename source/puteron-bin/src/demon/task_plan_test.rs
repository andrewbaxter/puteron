#![cfg(test)]
//! Notes:
//!
//! * Test setup sets on and transitive on. It sets started/state if it's stopped but
//!   on/transitive on only, i.e. external setup can pre-set the state to some state
//!   like starting/stopping.
use {
    super::{
        state::{
            StateDynamic,
            TaskStateEmpty,
            TaskStateLong,
            TaskStateShort,
            TaskStateSpecific,
            TaskState_,
        },
        task_plan::{
            plan_set_task_direct_off,
            plan_set_task_direct_on,
            ExecutePlan,
        },
        task_util::{
            get_task,
            walk_task_upstream,
        },
    },
    crate::demon::task_util::{
        is_task_started,
        is_task_stopped,
    },
    chrono::DateTime,
    puteron::interface::{
        ipc::ProcState,
        task::{
            Command,
            DependencyType,
            Environment,
            TaskSpecEmpty,
            TaskSpecLong,
            TaskSpecShort,
        },
    },
    std::cell::{
        Cell,
        RefCell,
    },
    tokio::sync::oneshot,
};

fn check<
    'a,
>(
    state_dynamic: &StateDynamic,
    plan: ExecutePlan,
    start: impl AsRef<[&'a str]>,
    stop: impl AsRef<[&'a str]>,
    started: impl AsRef<[&'a str]>,
    stopped: impl AsRef<[&'a str]>,
) {
    let mut errors = vec![];
    let expected_start = start.as_ref().iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let got_start = plan.start.iter().cloned().collect::<Vec<_>>();
    if got_start != expected_start {
        errors.push(format!("To start:\n  Expected: {:?}\n       Got: {:?}", expected_start, got_start));
    }
    let expected_stop = stop.as_ref().iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let got_stop = plan.stop.iter().cloned().collect::<Vec<_>>();
    if got_stop != expected_stop {
        errors.push(format!("To stop:\n  Expected: {:?}\n       Got: {:?}", expected_stop, got_stop));
    }
    for task_id in started.as_ref() {
        if !is_task_started(get_task(state_dynamic, &task_id.to_string())) {
            errors.push(format!("Expected [{}] started but isn't", task_id));
        }
    }
    for task_id in stopped.as_ref() {
        if !is_task_stopped(get_task(state_dynamic, &task_id.to_string())) {
            errors.push(format!("Expected [{}] stopped but isn't", task_id));
        }
    }
    if !errors.is_empty() {
        panic!("Errors:\n{}", errors.join("\n"));
    }
}

fn task(id: &str, specific: TaskStateSpecific) -> TaskState_ {
    return TaskState_ {
        id: id.to_string(),
        direct_on: Cell::new((match &specific {
            TaskStateSpecific::Empty(s) => s.spec.default_on,
            TaskStateSpecific::Long(s) => s.spec.default_on,
            TaskStateSpecific::Short(s) => s.spec.default_on,
        }, DateTime::UNIX_EPOCH)),
        transitive_on: Cell::new((false, DateTime::UNIX_EPOCH)),
        downstream: Default::default(),
        specific: specific,
        started_waiters: Default::default(),
        stopped_waiters: Default::default(),
    };
}

fn task_empty(
    id: &str,
    started: bool,
    upstream: impl IntoIterator<Item = (&'static str, DependencyType)>,
) -> TaskState_ {
    return task(id, TaskStateSpecific::Empty(TaskStateEmpty {
        started: Cell::new((started, DateTime::UNIX_EPOCH)),
        spec: TaskSpecEmpty {
            _schema: Default::default(),
            default_on: started,
            upstream: upstream.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        },
    }));
}

fn task_short(
    id: &str,
    on: bool,
    state: ProcState,
    upstream: impl AsRef<[(&'static str, DependencyType)]>,
) -> TaskState_ {
    match state {
        ProcState::Stopped => { },
        ProcState::Starting => {
            assert!(on);
        },
        ProcState::Started => { },
        ProcState::Stopping => {
            assert!(!on);
        },
    }
    return task(id, TaskStateSpecific::Short(TaskStateShort {
        state: Cell::new((state, DateTime::UNIX_EPOCH)),
        pid: Default::default(),
        failed_start_count: Default::default(),
        stop: match state {
            ProcState::Starting | ProcState::Started => RefCell::new(Some(oneshot::channel().0)),
            ProcState::Stopping | ProcState::Stopped => RefCell::new(None),
        },
        spec: TaskSpecShort {
            _schema: Default::default(),
            default_on: on,
            upstream: upstream.as_ref().into_iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            schedule: Default::default(),
            command: Command {
                working_directory: Default::default(),
                environment: Environment::default(),
                line: Default::default(),
            },
            success_codes: Default::default(),
            started_action: Default::default(),
            restart_delay: Default::default(),
            stop_timeout: Default::default(),
        },
    }));
}

fn task_long(
    id: &str,
    on: bool,
    state: ProcState,
    upstream: impl AsRef<[(&'static str, DependencyType)]>,
) -> TaskState_ {
    match state {
        ProcState::Stopped => { },
        ProcState::Starting => {
            assert!(on);
        },
        ProcState::Started => { },
        ProcState::Stopping => {
            assert!(!on);
        },
    }
    return task(id, TaskStateSpecific::Long(TaskStateLong {
        state: Cell::new((state, DateTime::UNIX_EPOCH)),
        pid: Default::default(),
        failed_start_count: Default::default(),
        stop: match state {
            ProcState::Starting | ProcState::Started => RefCell::new(Some(oneshot::channel().0)),
            ProcState::Stopping | ProcState::Stopped => RefCell::new(None),
        },
        spec: TaskSpecLong {
            _schema: Default::default(),
            default_on: on,
            upstream: upstream.as_ref().into_iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            command: Command {
                working_directory: Default::default(),
                environment: Environment::default(),
                line: Default::default(),
            },
            started_check: Default::default(),
            restart_delay: Default::default(),
            stop_timeout: Default::default(),
        },
    }));
}

fn build_state(tasks: impl IntoIterator<Item = TaskState_>) -> StateDynamic {
    let mut state_dynamic = StateDynamic {
        task_alloc: Default::default(),
        tasks: Default::default(),
        schedule_top: Default::default(),
        schedule: Default::default(),
        notify_reschedule: Default::default(),
    };
    for test_task in tasks.into_iter() {
        let id = test_task.id.clone();
        let task = state_dynamic.task_alloc.insert(test_task);
        state_dynamic.tasks.insert(id, task);
    }

    // Build downstream
    for task_id in state_dynamic.tasks.keys() {
        walk_task_upstream(get_task(&state_dynamic, task_id), |upstream| {
            for (upstream_id, upstream_type) in upstream {
                get_task(&state_dynamic, upstream_id)
                    .downstream
                    .borrow_mut()
                    .insert(task_id.clone(), *upstream_type);
            }
        })
    }

    // Set transitive_on
    for task_id in state_dynamic.tasks.keys() {
        let root_task = get_task(&state_dynamic, task_id);
        if !root_task.direct_on.get().0 {
            continue;
        }
        let mut frontier = vec![];

        fn inner<'a>(frontier: &mut Vec<&'a String>, task: &'a TaskState_) {
            walk_task_upstream(task, |upstream| {
                for (upstream_id, upstream_type) in upstream {
                    match upstream_type {
                        DependencyType::Strong => {
                            frontier.push(upstream_id);
                        },
                        DependencyType::Weak => {
                            continue;
                        },
                    }
                }
            })
        }

        inner(&mut frontier, &root_task);
        while let Some(task_id) = frontier.pop() {
            let task = get_task(&state_dynamic, task_id);
            task.transitive_on.set((true, DateTime::UNIX_EPOCH));
            inner(&mut frontier, task);
        }
    }

    // Done
    return state_dynamic;
}

#[test]
fn single_on() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", false, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_on(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], [], ["a"], []);
}

#[test]
fn single_on_proc() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, ProcState::Stopped, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_on(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, ["a"], [], [], []);
}

#[test]
fn single_on_noop() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", true, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_on(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], [], ["a"], []);
}

#[test]
fn single_off() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", true, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], [], [], ["a"]);
}

#[test]
fn single_off_proc() {
    let state_dynamic = build_state([
        //. .
        task_long("a", true, ProcState::Starting, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], ["a"], [], []);
}

#[test]
fn single_off_proc_short() {
    let state_dynamic = build_state([
        //. .
        task_short("a", true, ProcState::Started, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], [], [], ["a"]);
}

#[test]
fn single_off_noop() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, ProcState::Stopped, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], [], [], ["a"]);
}

#[test]
fn start_strong_upstream() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", false, []),
        task_empty("b", false, [("a", DependencyType::Strong)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], [], ["a", "b"], []);
}

#[test]
fn start_strong_upstream_proc() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", false, []),
        task_long("b", false, ProcState::Stopped, [("a", DependencyType::Strong)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, ["b"], [], ["a"], []);
}

#[test]
fn start_weak_downstream() {
    let state_dynamic = build_state([
        //. .
        task_empty("b", false, []),
        task_long("c", true, ProcState::Stopped, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, ["c"], [], ["b"], []);
}

#[test]
fn start_strong_upstream_weak_downstream_1() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, ProcState::Stopped, []),
        task_empty("b", false, [("a", DependencyType::Strong)]),
        task_long("c", true, ProcState::Stopped, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, ["a"], [], [], []);
}

#[test]
fn start_strong_upstream_weak_downstream_2() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", false, []),
        task_empty("b", false, [("a", DependencyType::Strong)]),
        task_long("c", true, ProcState::Stopped, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, ["c"], [], ["a", "b"], []);
}

#[test]
fn stop_strong_upstream() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, ProcState::Started, []),
        task_empty("b", true, [("a", DependencyType::Strong)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], ["a"], [], ["b"]);
}

#[test]
fn stop_weak_downstream() {
    let state_dynamic = build_state([
        //. .
        task_long("b", true, ProcState::Started, []),
        task_empty("c", true, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], ["b"], [], ["c"]);
}

#[test]
fn strong_downstream_dont_stop() {
    let state_dynamic = build_state([
        //. .
        task_long("b", true, ProcState::Started, []),
        task_empty("c", true, [("b", DependencyType::Strong)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], [], [], []);
}

#[test]
fn stop_strong_upstream_weak_downstream_1() {
    let state_dynamic = build_state([
        //. .
        task_long("a", true, ProcState::Started, []),
        task_empty("b", true, [("a", DependencyType::Strong)]),
        task_long("c", true, ProcState::Started, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], ["c"], ["a", "b"], []);
}

#[test]
fn stop_strong_upstream_weak_downstream_2() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, ProcState::Started, []),
        task_empty("b", true, [("a", DependencyType::Strong)]),
        task_empty("c", true, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], ["a"], [], ["b", "c"]);
}
