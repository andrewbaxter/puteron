use {
    super::state::{
        StateDynamic,
        TaskStateSpecific,
        TaskState_,
    },
    crate::interface::{
        self,
        base::TaskId,
        ipc::{
            Actual,
            Event,
            EventType,
        },
        task::{
            DependencyType,
            ShortTaskStartedAction,
            TaskSpecShort,
        },
    },
    chrono::Utc,
};

pub(crate) fn actual_set(state_dynamic: &StateDynamic, task: &TaskState_, actual: Actual) {
    eprintln!("      =actual {} {:?}", task.id, actual);
    task.actual.set((actual, Utc::now()));
    let sender = state_dynamic.watchers_send.borrow_mut().take();
    if let Some(sender) = sender {
        _ = sender.send(Event {
            task: task.id.clone(),
            event: EventType::Actual(actual),
        }) as Result<_, _>;
        *state_dynamic.watchers_send.borrow_mut() = Some(sender);
    }
}

pub(crate) fn transitive_on_set(state_dynamic: &StateDynamic, task: &TaskState_, on: bool) {
    let was_effective_on = is_control_effective_on(task);
    eprintln!("      =transitive_on {} {:?}", task.id, on);
    task.transitive_on.set((on, Utc::now()));
    let sender = state_dynamic.watchers_send.borrow_mut().take();
    if let Some(sender) = sender {
        _ = sender.send(Event {
            task: task.id.clone(),
            event: EventType::TransitiveOn(on),
        }) as Result<_, _>;
        let is_effective_on = is_control_effective_on(task);
        if was_effective_on != is_effective_on {
            _ = sender.send(Event {
                task: task.id.clone(),
                event: EventType::EffectiveOn(is_effective_on),
            }) as Result<_, _>;
        }
        *state_dynamic.watchers_send.borrow_mut() = Some(sender);
    }
}

pub(crate) fn direct_on_set(state_dynamic: &StateDynamic, task: &TaskState_, on: bool) {
    let was_effective_on = is_control_effective_on(task);
    eprintln!("      =direct_on {} {:?}", task.id, on);
    task.direct_on.set((on, Utc::now()));
    let sender = state_dynamic.watchers_send.borrow_mut().take();
    if let Some(sender) = sender {
        _ = sender.send(Event {
            task: task.id.clone(),
            event: EventType::DirectOn(on),
        }) as Result<_, _>;
        let is_effective_on = is_control_effective_on(task);
        if was_effective_on != is_effective_on {
            _ = sender.send(Event {
                task: task.id.clone(),
                event: EventType::EffectiveOn(is_effective_on),
            }) as Result<_, _>;
        }
        *state_dynamic.watchers_send.borrow_mut() = Some(sender);
    }
}

pub(crate) fn awueo_set(state_dynamic: &StateDynamic, task: &TaskState_, on: bool) {
    let was_effective_on = is_control_effective_on(task);
    eprintln!("      =awueo {} {:?}", task.id, on);
    task.awueo.set(on);
    let sender = state_dynamic.watchers_send.borrow_mut().take();
    if let Some(sender) = sender {
        let is_effective_on = is_control_effective_on(task);
        if was_effective_on != is_effective_on {
            _ = sender.send(Event {
                task: task.id.clone(),
                event: EventType::EffectiveOn(is_effective_on),
            });
        }
        *state_dynamic.watchers_send.borrow_mut() = Some(sender);
    }
}

pub(crate) fn walk_task_upstream<
    'a,
    T,
>(t: &'a TaskState_, mut cb: impl FnMut(&mut dyn Iterator<Item = (&'a TaskId, &'a DependencyType)>) -> T) -> T {
    match &t.specific {
        TaskStateSpecific::Empty(s) => return cb(&mut s.spec.upstream.iter()),
        TaskStateSpecific::Long(s) => return cb(&mut s.spec.upstream.iter()),
        TaskStateSpecific::Short(s) => return cb(&mut s.spec.upstream.iter()),
    }
}

pub(crate) fn get_task<'a>(state_dynamic: &'a StateDynamic, task_id: &TaskId) -> &'a TaskState_ {
    return &state_dynamic.task_alloc[*state_dynamic.tasks.get(task_id).unwrap()];
}

pub(crate) fn maybe_get_task<'a>(state_dynamic: &'a StateDynamic, task_id: &TaskId) -> Option<&'a TaskState_> {
    let Some(t) = state_dynamic.tasks.get(task_id) else {
        return None;
    };
    return Some(&state_dynamic.task_alloc[*t]);
}

pub(crate) fn is_control_effective_on(t: &TaskState_) -> bool {
    return t.awueo.get() && (t.direct_on.get().0 || t.transitive_on.get().0);
}

pub(crate) fn get_short_task_started_action(specific: &TaskSpecShort) -> ShortTaskStartedAction {
    return match specific.started_action {
        None => {
            if specific.schedule.is_empty() {
                interface::task::ShortTaskStartedAction::None
            } else {
                interface::task::ShortTaskStartedAction::TurnOff
            }
        },
        Some(a) => a,
    };
}

pub(crate) fn is_actual_all_upstream_tasks_started(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    let mut all_started = true;
    walk_task_upstream(task, |deps| {
        for (task_id, _) in deps {
            let Some(dep) = state_dynamic.tasks.get(task_id) else {
                all_started = false;
                return;
            };
            if state_dynamic.task_alloc[*dep].actual.get().0 != Actual::Started {
                all_started = false;
                return;
            }
        }
    });
    return all_started;
}

pub(crate) fn are_all_downstream_tasks_stopped(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    for (task_id, _) in task.downstream.borrow().iter() {
        if get_task(state_dynamic, task_id).actual.get().0 != Actual::Stopped {
            return false;
        }
    }
    return true;
}

pub(crate) fn are_all_strong_downstream_direct_or_transitive_off(
    state_dynamic: &StateDynamic,
    task: &TaskState_,
) -> bool {
    for (downstream_id, downstream_type) in task.downstream.borrow().iter() {
        if *downstream_type != DependencyType::Strong {
            continue;
        }
        let downstream_task = get_task(state_dynamic, downstream_id);
        if !(downstream_task.direct_on.get().0 || downstream_task.transitive_on.get().0) {
            return false;
        }
    }
    return true;
}

pub(crate) fn are_all_weak_upstream_effective_on(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    let mut all = true;
    walk_task_upstream(task, |upstream| {
        for (upstream_id, upstream_type) in upstream {
            if *upstream_type != DependencyType::Weak {
                continue;
            }
            let effective_on = is_control_effective_on(&get_task(state_dynamic, upstream_id));
            if !effective_on {
                all = false;
                break;
            }
        }
    });
    return all;
}
