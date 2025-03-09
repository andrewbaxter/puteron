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
        task_control::{
            plan_event_stopped,
            plan_set_direct,
            plan_set_direct_off,
            plan_set_direct_on,
            ExecutePlan,
        },
        task_util::{
            get_task,
            walk_task_upstream,
        },
    },
    crate::interface::{
        ipc::Actual,
        task::{
            Command,
            DependencyType,
            Environment,
            TaskSpecEmpty,
            TaskSpecLong,
            TaskSpecShort,
        },
    },
    chrono::DateTime,
    std::cell::{
        Cell,
        RefCell,
    },
    tokio::sync::{
        oneshot,
    },
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
    eprintln!("Plan: {:?}", plan);
    let mut errors = vec![];
    let expected_start = start.as_ref().iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let got_start = plan.run.iter().cloned().collect::<Vec<_>>();
    if got_start != expected_start {
        errors.push(format!("To start:\n  Expected: {:?}\n       Got: {:?}", expected_start, got_start));
    }
    let expected_stop = stop.as_ref().iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let got_stop = plan.stop.iter().cloned().collect::<Vec<_>>();
    if got_stop != expected_stop {
        errors.push(format!("To stop:\n  Expected: {:?}\n       Got: {:?}", expected_stop, got_stop));
    }
    for task_id in started.as_ref() {
        if get_task(state_dynamic, &task_id.to_string()).actual.get().0 != Actual::Started {
            errors.push(format!("Expected [{}] started but isn't", task_id));
        }
    }
    for task_id in stopped.as_ref() {
        if get_task(state_dynamic, &task_id.to_string()).actual.get().0 != Actual::Stopped {
            errors.push(format!("Expected [{}] stopped but isn't", task_id));
        }
    }
    if !errors.is_empty() {
        panic!("Errors:\n{}", errors.join("\n"));
    }
}

fn task(id: &str, actual: Actual, specific: TaskStateSpecific) -> TaskState_ {
    let direct_on;
    let awueo;
    match &specific {
        TaskStateSpecific::Empty(s) => {
            direct_on = s.spec.default_on;
            awueo = s.spec.upstream.values().all(|t| *t == DependencyType::Strong);
        },
        TaskStateSpecific::Long(s) => {
            direct_on = s.spec.default_on;
            awueo = s.spec.upstream.values().all(|t| *t == DependencyType::Strong);
        },
        TaskStateSpecific::Short(s) => {
            direct_on = s.spec.default_on;
            awueo = s.spec.upstream.values().all(|t| *t == DependencyType::Strong);
        },
    }
    return TaskState_ {
        id: id.to_string(),
        direct_on: Cell::new((direct_on, DateTime::UNIX_EPOCH)),
        // placeholder, calculated later
        transitive_on: Cell::new((false, DateTime::UNIX_EPOCH)),
        // placeholder, calculated later
        awueo: Cell::new(awueo),
        actual: Cell::new((actual, DateTime::UNIX_EPOCH)),
        // placeholder, calculated later
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
    return task(id, if started {
        Actual::Started
    } else {
        Actual::Stopped
    }, TaskStateSpecific::Empty(TaskStateEmpty { spec: TaskSpecEmpty {
        _schema: Default::default(),
        default_on: started,
        upstream: upstream.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    } }));
}

fn task_short(
    id: &str,
    on: bool,
    actual: Actual,
    upstream: impl AsRef<[(&'static str, DependencyType)]>,
) -> TaskState_ {
    match actual {
        Actual::Stopped => { },
        Actual::Starting => {
            assert!(on);
        },
        Actual::Started => { },
        Actual::Stopping => {
            assert!(!on);
        },
    }
    return task(id, actual, TaskStateSpecific::Short(TaskStateShort {
        pid: Default::default(),
        failed_start_count: Default::default(),
        stop: match actual {
            Actual::Starting | Actual::Started => RefCell::new(Some(oneshot::channel().0)),
            Actual::Stopping | Actual::Stopped => RefCell::new(None),
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
            restart_delay_max: Default::default(),
            stop_timeout: Default::default(),
        },
    }));
}

fn task_long(
    id: &str,
    on: bool,
    actual: Actual,
    upstream: impl AsRef<[(&'static str, DependencyType)]>,
) -> TaskState_ {
    match actual {
        Actual::Stopped => { },
        Actual::Starting => {
            assert!(on);
        },
        Actual::Started => { },
        Actual::Stopping => {
            assert!(!on);
        },
    }
    return task(id, actual, TaskStateSpecific::Long(TaskStateLong {
        pid: Default::default(),
        failed_start_count: Default::default(),
        stop: match actual {
            Actual::Starting | Actual::Started => RefCell::new(Some(oneshot::channel().0)),
            Actual::Stopping | Actual::Stopped => RefCell::new(None),
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
            restart_delay_max: Default::default(),
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
        idle_watchers: Default::default(),
        watchers_send: Default::default(),
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

    // Adjust automatic `on` states
    for task_id in state_dynamic.tasks.keys() {
        let task = get_task(&state_dynamic, task_id);
        if task.direct_on.get().0 {
            plan_set_direct(&state_dynamic, task_id, true, |_task| { });
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
    plan_set_direct_on(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], [], ["a"], []);
}

#[test]
fn single_on_proc() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, Actual::Stopped, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_on(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, ["a"], [], [], []);
}

#[test]
fn single_on_noop() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", true, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_on(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], [], ["a"], []);
}

#[test]
fn single_off() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", true, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], [], [], ["a"]);
}

#[test]
fn single_off_proc() {
    let state_dynamic = build_state([
        //. .
        task_long("a", true, Actual::Starting, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], ["a"], [], []);
}

#[test]
fn single_off_proc_short() {
    let state_dynamic = build_state([
        //. .
        task_short("a", true, Actual::Started, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(&state_dynamic, &mut plan, &"a".to_string());
    check(&state_dynamic, plan, [], [], [], ["a"]);
}

#[test]
fn single_off_noop() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, Actual::Stopped, []),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(&state_dynamic, &mut plan, &"a".to_string());
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
    plan_set_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], [], ["a", "b"], []);
}

#[test]
fn start_strong_upstream_proc() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", false, []),
        task_long("b", false, Actual::Stopped, [("a", DependencyType::Strong)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, ["b"], [], ["a"], []);
}

#[test]
fn start_weak_downstream() {
    let state_dynamic = build_state([
        //. .
        task_empty("b", false, []),
        task_long("c", true, Actual::Stopped, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, ["c"], [], ["b"], []);
}

#[test]
fn start_strong_upstream_weak_downstream_1() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, Actual::Stopped, []),
        task_empty("b", false, [("a", DependencyType::Strong)]),
        task_long("c", true, Actual::Stopped, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, ["a"], [], [], []);
}

#[test]
fn start_strong_upstream_weak_downstream_2() {
    let state_dynamic = build_state([
        //. .
        task_empty("a", false, []),
        task_empty("b", false, [("a", DependencyType::Strong)]),
        task_long("c", true, Actual::Stopped, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_on(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, ["c"], [], ["a", "b"], []);
}

#[test]
fn stop_strong_upstream() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, Actual::Started, []),
        task_empty("b", true, [("a", DependencyType::Strong)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], ["a"], [], ["b"]);
}

#[test]
fn stop_weak_downstream() {
    let state_dynamic = build_state([
        //. .
        task_long("b", true, Actual::Started, []),
        task_empty("c", true, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], ["b"], [], ["c"]);
}

#[test]
fn strong_downstream_dont_stop() {
    let state_dynamic = build_state([
        //. .
        task_long("b", true, Actual::Started, []),
        task_empty("c", true, [("b", DependencyType::Strong)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], [], [], []);
}

#[test]
fn stop_strong_upstream_weak_downstream_1() {
    let state_dynamic = build_state([
        //. .
        task_long("a", true, Actual::Started, []),
        task_empty("b", true, [("a", DependencyType::Strong)]),
        task_long("c", true, Actual::Started, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], ["c"], ["a", "b"], []);
}

#[test]
fn stop_strong_upstream_weak_downstream_2() {
    let state_dynamic = build_state([
        //. .
        task_long("a", false, Actual::Started, []),
        task_empty("b", true, [("a", DependencyType::Strong)]),
        task_empty("c", true, [("b", DependencyType::Weak)]),
    ]);
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], ["a"], [], ["b", "c"]);
}

#[test]
fn stopped_weak_downstream() {
    // a is turned off and thus all weak downstream (b) are triggered to stop. b
    // stops, but the stop isn't propagated since b is still on. Instead of "on", an
    // "effective on" should be checked which incorporates the on status of weak
    // upstreams.
    let state_dynamic = build_state([
        //. .
        task_long("a", false, Actual::Started, []),
        task_empty("b", true, [("a", DependencyType::Weak)]),
    ]);
    let b = get_task(&state_dynamic, &"b".to_string());
    assert!(!b.awueo.get());
    let mut plan = ExecutePlan::default();
    get_task(&state_dynamic, &"b".to_string()).actual.set((Actual::Stopped, DateTime::UNIX_EPOCH));
    plan_event_stopped(&state_dynamic, &mut plan, &"b".to_string());
    check(&state_dynamic, plan, [], ["a"], [], ["b"]);
}

// Offing a strong downstream should cause weak sibling on downstreams to be
// stopped.
#[test]
fn test_zigzag_stop_weak_downstream() {
    let state_dynamic = build_state([
        //. .
        task_long("a", true, Actual::Started, []),
        task_empty("b", true, [("a", DependencyType::Strong)]),
        task_long("c", true, Actual::Started, [("a", DependencyType::Weak)]),
    ]);
    let a = get_task(&state_dynamic, &"a".to_string());
    let c = get_task(&state_dynamic, &"c".to_string());
    assert!(a.transitive_on.get().0);
    assert!(c.awueo.get());
    a.direct_on.set((false, DateTime::UNIX_EPOCH));

    // `a` transitive_on still true
    //
    // First stop `b`
    {
        let mut plan = ExecutePlan::default();
        plan_set_direct_off(&state_dynamic, &mut plan, &"b".to_string());
        check(&state_dynamic, plan, [], ["c"], [], ["b"]);
        assert!(!a.transitive_on.get().0);
        assert!(!c.awueo.get());
    }

    // Now `c` stops.
    {
        let c = get_task(&state_dynamic, &"c".to_string());
        c.actual.set((Actual::Stopped, DateTime::UNIX_EPOCH));
        let mut plan = ExecutePlan::default();
        plan_event_stopped(&state_dynamic, &mut plan, &"c".to_string());
        check(&state_dynamic, plan, [], ["a"], [], ["b", "c"]);
    }
}
