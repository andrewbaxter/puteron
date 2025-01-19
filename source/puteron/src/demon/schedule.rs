use {
    super::state::StateDynamic,
    crate::demon::state::TaskStateSpecific,
    chrono::{
        DateTime,
        Datelike,
        Months,
        Timelike,
        Utc,
    },
    puteron_lib::interface::{
        self,
        base::TaskId,
    },
    rand::{
        thread_rng,
        Rng,
    },
    std::{
        collections::BTreeMap,
        sync::Arc,
        time::Duration,
    },
    tokio::time::Instant,
};

pub(crate) type ScheduleRule = Arc<(TaskId, interface::task::schedule::Rule)>;
pub(crate) type ScheduleDynamic = BTreeMap<Instant, Vec<ScheduleRule>>;

pub fn calc_next_instant(
    now: DateTime<Utc>,
    instant_now: Instant,
    schedule: &interface::task::schedule::Rule,
    // At startup, for scattered rules
    initial: bool,
) -> Instant {
    let mut next;
    match schedule {
        interface::task::schedule::Rule::Period(s) => {
            if initial && s.scattered {
                return instant_now +
                    Duration::from_secs_f64(
                        Duration::from(s.period.into()).as_secs_f64() * thread_rng().gen_range::<f64, _>(0. .. 1.),
                    );
            } else {
                return instant_now + s.period.into();
            }
        },
        interface::task::schedule::Rule::Hourly(s) => {
            next = now.with_minute(s.minute as u32).unwrap().with_second(s.second as u32).unwrap();
            if next < now {
                next += chrono::Duration::hours(1);
            }
        },
        interface::task::schedule::Rule::Daily(s) => {
            next = now.with_time(*s).unwrap();
            if next < now {
                next += chrono::Duration::days(1);
            }
        },
        interface::task::schedule::Rule::Weekly(s) => {
            next =
                now.with_time(s.time).unwrap() +
                    chrono::Duration::days(
                        s.weekday.num_days_from_monday() as i64 - now.weekday().num_days_from_monday() as i64,
                    );
            if next < now {
                next += chrono::Duration::days(7);
            }
        },
        interface::task::schedule::Rule::Monthly(s) => {
            next = match now.date_naive().with_day(s.day as u32) {
                Some(n) => n,
                None => now
                    .date_naive()
                    .with_day(1)
                    .unwrap()
                    .checked_add_months(Months::new(1))
                    .unwrap()
                    .pred_opt()
                    .unwrap(),
            }.and_time(s.time).and_utc();
            if next < now {
                next = next.checked_add_months(Months::new(1)).unwrap();
            }
        },
        interface::task::schedule::Rule::Yearly(s) => {
            let next1 = now.date_naive().with_month(s.month.0.number_from_month()).unwrap();
            next = match next1.with_day(s.day as u32) {
                Some(n) => n,
                None => next1.with_day(1).unwrap().checked_add_months(Months::new(1)).unwrap().pred_opt().unwrap(),
            }.and_time(s.time).and_utc();
            if next < now {
                next = next.checked_add_months(Months::new(12)).unwrap();
            }
        },
    }
    return instant_now + (next - now).to_std().unwrap();
}

pub(crate) fn populate_schedule(state_dynamic: &mut StateDynamic) {
    for (id, task) in &state_dynamic.tasks {
        let task = &state_dynamic.task_alloc[*task];
        if let TaskStateSpecific::Short(t) = &task.specific {
            for rule in &t.spec.schedule {
                state_dynamic
                    .schedule
                    .entry(calc_next_instant(Utc::now(), Instant::now(), rule, false))
                    .or_default()
                    .push(ScheduleRule::new((id.clone(), rule.clone())));
            }
        }
    }
}

pub(crate) fn pop_schedule(state_dynamic: &mut StateDynamic) -> (Instant, ScheduleRule) {
    let mut next_entry = state_dynamic.schedule.first_entry().unwrap();
    let instant = next_entry.key().clone();
    let next_tasks = next_entry.get_mut();
    let spec = next_tasks.pop().unwrap();
    if next_tasks.is_empty() {
        next_entry.remove();
    }
    state_dynamic.schedule_top = Some((instant, spec.clone()));
    return (instant, spec);
}
