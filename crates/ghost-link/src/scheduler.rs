//! Local schedule driver: recurring prompts that run as ordinary agent turns.
//!
//! Phase 5 of the local assistant. A schedule is a row here plus a timer; each
//! firing produces a normal agent turn with that workspace's capabilities, which
//! means the capability gate and the approval queue apply to scheduled work
//! exactly as they do to interactive chat. A scheduled run is not a privileged
//! path.
//!
//! Design choices that matter:
//!
//! - **Cron is a documented 5-field subset, not a general parser.** With no new
//!   dependency, a partial cron implementation is the obvious temptation, and the
//!   failure mode of getting it subtly wrong is a job that silently never fires.
//!   [`CronSpec::parse`] therefore rejects anything it does not fully implement,
//!   at insert time, so the user finds out immediately rather than by noticing a
//!   job that stopped running.
//! - **Persistence is a JSON file**, matching `sessions.json` / `approvals.json`
//!   in this crate. A schedule table is tens of rows; adding SQLite to the server
//!   binary for it would be more machinery than the data justifies.
//! - **Missed runs are not backfilled.** A laptop that was asleep does not wake
//!   up wanting thirty catch-up executions of a prompt that writes files. The next
//!   firing is simply the next one.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

// `DateTime::day`/`hour`/`minute`/`weekday` come from chrono's clock trait; the
// derive-based accessors are only on `NaiveDateTime`. Importing `Datelike` for
// the calendar fields and `Timelike` for the time-of-day ones.
use chrono::{Datelike, Timelike};

use serde::{Deserialize, Serialize};

/// Longest prompt retained. Bounds the file and keeps a runaway prompt from
/// being the whole schedule store.
const MAX_PROMPT_CHARS: usize = 8000;

/// When a schedule fires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum Trigger {
    /// Standard 5-field cron: minute hour day-of-month month day-of-week.
    Cron(String),
    /// One-shot, at a unix timestamp (seconds).
    At(i64),
}

impl Trigger {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cron(_) => "cron",
            Self::At(_) => "at",
        }
    }
}

/// The outcome of the most recent run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum RunStatus {
    /// Never run. The default, so a record written before this field existed
    /// loads as "hasn't run yet" rather than failing to deserialize.
    #[default]
    Pending,
    /// Ran and produced a text answer.
    Ok,
    /// Ran and errored.
    Failed(String),
    /// Produced at least one write/exec call that is waiting on approval, so the
    /// turn is only partly complete.
    NeedsApproval(String),
}

impl RunStatus {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ok => "ok",
            Self::Failed(_) => "failed",
            Self::NeedsApproval(_) => "needs_approval",
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// One scheduled prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub workspace_id: String,
    /// Human label shown in the GUI.
    pub name: String,
    pub prompt: String,
    pub trigger: Trigger,
    pub enabled: bool,
    /// Unix seconds of the last firing, or `None` if never run.
    #[serde(default)]
    pub last_run: Option<i64>,
    #[serde(default)]
    pub last_status: RunStatus,
    /// Whether the last run produced only observational tool calls, which is
    /// what makes a schedule eligible to auto-run without a human present.
    #[serde(default)]
    pub last_run_was_read_only: bool,
    #[serde(default)]
    pub created_at: i64,
}

impl Schedule {
    /// The next firing at or after `from`, or `None` for a disabled schedule or
    /// a cron spec this parser doesn't implement.
    pub fn next_fire_after(&self, from: i64) -> Option<i64> {
        if !self.enabled {
            return None;
        }
        match &self.trigger {
            Trigger::At(ts) => Some(*ts).filter(|t| *t > from),
            Trigger::Cron(spec) => CronSpec::parse(spec).ok()?.next_after(from),
        }
    }

    /// The list-facing view. Carries no prompt body by design: the GUI gets names,
    /// timing, and status, and the prompt is fetched only if the user asks.
    ///
    /// This mirrors `ActionSummary` — keeping free text out of list payloads is
    /// what stops a prompt (which may quote a file or a secret the user pasted)
    /// from being served to anyone who can list schedules.
    pub fn to_summary(&self) -> ScheduleSummary {
        ScheduleSummary {
            id: self.id.clone(),
            workspace_id: self.workspace_id.clone(),
            name: self.name.clone(),
            trigger: self.trigger.kind().to_string(),
            trigger_value: match &self.trigger {
                Trigger::Cron(s) => s.clone(),
                Trigger::At(t) => t.to_string(),
            },
            enabled: self.enabled,
            last_run: self.last_run,
            last_status: self.last_status.kind().to_string(),
            last_run_was_read_only: self.last_run_was_read_only,
            next_run: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScheduleSummary {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub trigger: String,
    pub trigger_value: String,
    pub enabled: bool,
    pub last_run: Option<i64>,
    pub last_status: String,
    pub last_run_was_read_only: bool,
    pub next_run: Option<i64>,
}

/// A parsed 5-field cron expression.
///
/// Supports `*`, `a`, `a-b`, `a,b,c`, and `*/n` in every field. Day-of-week
/// accepts 0-7 (7 = Sunday). Rejects anything else rather than guessing, because
/// a cron parser that silently misfires is worse than one that refuses.
#[derive(Debug, Clone, PartialEq)]
pub struct CronSpec {
    minute: Field,
    hour: Field,
    dom: Field,
    month: Field,
    dow: Field,
}

#[derive(Debug, Clone, PartialEq)]
struct Field {
    bits: u64,
    min: u32,
    max: u32,
}

impl Field {
    fn parse(spec: &str, min: u32, max: u32, name: &str) -> Result<Self, String> {
        let mut bits = 0u64;
        for part in spec.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(format!("empty {name} field entry"));
            }
            // `*/n`
            let (range, step) = match part.split_once('/') {
                Some((r, s)) => {
                    let step: u32 = s
                        .parse()
                        .map_err(|_| format!("bad step '{s}' in {name} field"))?;
                    if step == 0 {
                        return Err(format!("zero step in {name} field"));
                    }
                    (r, step)
                }
                None => (part, 1),
            };
            let (lo, hi) = if range == "*" {
                (min, max)
            } else if let Some((a, b)) = range.split_once('-') {
                let a: u32 = a
                    .parse()
                    .map_err(|_| format!("bad value '{a}' in {name} field"))?;
                let b: u32 = b
                    .parse()
                    .map_err(|_| format!("bad value '{b}' in {name} field"))?;
                (a, b)
            } else {
                let a: u32 = range
                    .parse()
                    .map_err(|_| format!("bad value '{range}' in {name} field"))?;
                (a, a)
            };
            if lo < min || hi > max || lo > hi {
                return Err(format!(
                    "{name} field value out of range ({min}-{max}): {range}"
                ));
            }
            let mut v = lo;
            while v <= hi {
                bits |= 1u64 << v;
                v += step;
            }
        }
        Ok(Self { bits, min, max })
    }

    fn matches(&self, value: u32) -> bool {
        self.bits & (1u64 << value) != 0
    }
}

impl CronSpec {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let fields: Vec<&str> = spec.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "expected 5 fields (minute hour day-of-month month day-of-week), got {}",
                fields.len()
            ));
        }
        Ok(Self {
            minute: Field::parse(fields[0], 0, 59, "minute")?,
            hour: Field::parse(fields[1], 0, 23, "hour")?,
            dom: Field::parse(fields[2], 1, 31, "day-of-month")?,
            month: Field::parse(fields[3], 1, 12, "month")?,
            dow: Field::parse(fields[4], 0, 7, "day-of-week")?,
        })
    }

    /// The next firing strictly after `from` (unix seconds, UTC), or `None`
    /// within a four-year horizon.
    ///
    /// Bounded search rather than unbounded: a spec like `0 0 30 2 *`
    /// (February 30th) can never fire, and an unbounded loop would hang the
    /// scheduler task forever.
    pub fn next_after(&self, from: i64) -> Option<i64> {
        // Four years covers every Feb-29 gap, so a spec that can fire is found.
        let horizon = from + 4 * 366 * 24 * 3600;
        let mut t = from - from.rem_euclid(60) + 60; // next whole minute
        while t <= horizon {
            let dt = chrono::DateTime::from_timestamp(t, 0)?;
            // Unix weekday is 0=Thursday; chrono gives 1=Monday..7=Sunday.
            let dow_unix = (dt.weekday().num_days_from_monday() + 3) % 7;
            let dom_restricted = self.dom.bits != all_bits(1, 31);
            let dow_restricted = self.dow.bits != (all_bits(0, 6) | (1 << 7));
            // Standard cron quirk: when both day fields are restricted, either
            // matching is enough; otherwise both must match.
            let day_ok = match (dom_restricted, dow_restricted) {
                (true, true) => self.dom.matches(dt.day()) || self.dow.matches(dow_unix),
                (true, false) => self.dom.matches(dt.day()),
                (false, true) => self.dow.matches(dow_unix),
                (false, false) => true,
            };
            if self.minute.matches(dt.minute())
                && self.hour.matches(dt.hour())
                && self.month.matches(dt.month())
                && day_ok
            {
                return Some(t);
            }
            t += 60;
        }
        None
    }
}

fn all_bits(min: u32, max: u32) -> u64 {
    let mut bits = 0u64;
    for v in min..=max {
        bits |= 1u64 << v;
    }
    bits
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScheduleFile {
    #[serde(default)]
    schedules: Vec<Schedule>,
}

/// Persistent, workspace-scoped schedule store.
#[derive(Debug)]
pub struct ScheduleStore {
    path: PathBuf,
    schedules: Mutex<Vec<Schedule>>,
}

impl ScheduleStore {
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let schedules = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<ScheduleFile>(&raw).ok())
            .map(|f| f.schedules)
            .unwrap_or_default();
        Self {
            path,
            schedules: Mutex::new(schedules),
        }
    }

    fn persist(&self, schedules: &[Schedule]) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = ScheduleFile {
            schedules: schedules.to_vec(),
        };
        match serde_json::to_string_pretty(&file) {
            Ok(json) => {
                if let Err(err) = std::fs::write(&self.path, json) {
                    tracing::warn!("failed to persist schedules: {err}");
                }
            }
            Err(err) => tracing::warn!("failed to serialize schedules: {err}"),
        }
    }

    /// Adds a schedule, validating the trigger up front.
    ///
    /// Rejecting an unparseable cron *here* is the whole point: a schedule that
    /// can never fire should fail loudly at creation, not silently never run.
    pub fn add(
        &self,
        workspace_id: &str,
        name: &str,
        prompt: &str,
        trigger: Trigger,
    ) -> Result<Schedule, String> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("prompt is required".to_string());
        }
        if prompt.chars().count() > MAX_PROMPT_CHARS {
            return Err(format!(
                "prompt too long ({} chars, max {MAX_PROMPT_CHARS})",
                prompt.chars().count()
            ));
        }
        if let Trigger::Cron(spec) = &trigger {
            CronSpec::parse(spec).map_err(|e| format!("invalid cron '{spec}': {e}"))?;
        }
        let schedule = Schedule {
            id: uuid::Uuid::new_v4().to_string(),
            workspace_id: workspace_id.to_string(),
            name: if name.trim().is_empty() {
                "unnamed schedule".to_string()
            } else {
                name.trim().to_string()
            },
            prompt: prompt.to_string(),
            trigger,
            enabled: true,
            last_run: None,
            last_status: RunStatus::Pending,
            last_run_was_read_only: false,
            created_at: now_secs(),
        };
        let mut guard = self.schedules.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(schedule.clone());
        self.persist(&guard);
        Ok(schedule)
    }

    pub fn remove(&self, workspace_id: &str, id: &str) -> bool {
        let mut guard = self.schedules.lock().unwrap_or_else(|e| e.into_inner());
        let before = guard.len();
        guard.retain(|s| !(s.id == id && s.workspace_id == workspace_id));
        let removed = guard.len() < before;
        if removed {
            self.persist(&guard);
        }
        removed
    }

    pub fn set_enabled(&self, workspace_id: &str, id: &str, enabled: bool) -> bool {
        let mut guard = self.schedules.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = guard
            .iter_mut()
            .find(|s| s.id == id && s.workspace_id == workspace_id)
        else {
            return false;
        };
        s.enabled = enabled;
        self.persist(&guard);
        true
    }

    /// Every schedule for one workspace.
    pub fn list(&self, workspace_id: &str) -> Vec<Schedule> {
        let guard = self.schedules.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .filter(|s| s.workspace_id == workspace_id)
            .cloned()
            .collect()
    }

    /// One schedule, scoped to its workspace.
    ///
    /// Returns `None` for another workspace's id rather than a redaction, so the
    /// endpoint can't be used to enumerate ids.
    pub fn get(&self, workspace_id: &str, id: &str) -> Option<Schedule> {
        let guard = self.schedules.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .find(|s| s.id == id && s.workspace_id == workspace_id)
            .cloned()
    }

    /// Records the outcome of a run.
    pub fn record_run(&self, workspace_id: &str, id: &str, status: RunStatus, read_only: bool) {
        let mut guard = self.schedules.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = guard
            .iter_mut()
            .find(|s| s.id == id && s.workspace_id == workspace_id)
        {
            s.last_run = Some(now_secs());
            s.last_status = status;
            s.last_run_was_read_only = read_only;
            self.persist(&guard);
        }
    }

    /// Enabled schedules due at or before `now`.
    ///
    /// A schedule that has never run is due immediately, whatever its trigger
    /// says — otherwise a schedule created for 09:00 would sit idle until 09:00
    /// the next day, which is not what "create this schedule" means to anyone.
    /// `next_fire_after` therefore anchors a never-run schedule at the epoch
    /// rather than at `created_at`.
    ///
    /// This is deliberately not backfill: a schedule missed while the machine was
    /// asleep fires once on restart, and its `last_run` moves to now, so the
    /// missed occurrences are dropped rather than replayed.
    pub fn due(&self, now: i64) -> Vec<Schedule> {
        let guard = self.schedules.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        for s in guard.iter().filter(|s| s.enabled) {
            if s.last_run.is_none() {
                out.push(s.clone());
                continue;
            }
            let next = s.next_fire_after(s.last_run.unwrap_or(0));
            let Some(next) = next else { continue };
            if next <= now {
                out.push(s.clone());
            }
        }
        out
    }

    /// Next firing across all schedules, for the timer's sleep duration.
    /// Next firing across all enabled schedules, for the driver's sleep.
    ///
    /// `now` is deliberately unused: the answer is a property of the schedules
    /// and their last runs, not of the current time, and taking it would imply a
    /// dependency the computation doesn't have.
    pub fn next_wakeup(&self, _now: i64) -> Option<i64> {
        let guard = self.schedules.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .filter(|s| s.enabled)
            .filter_map(|s| s.next_fire_after(s.last_run.unwrap_or(0)))
            .min()
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Workspace resolution for a schedule request, matching the approval queue.
pub fn requested_workspace(requested: Option<&str>) -> String {
    match requested {
        Some(id) => crate::workspace::sanitize_id(id),
        None => crate::active_workspace().id().to_string(),
    }
}

/// Formats a unix timestamp for display in the GUI.
pub fn format_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "invalid timestamp".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> ScheduleStore {
        ScheduleStore::open(dir.path().join("schedules.json"))
    }

    // 2024-03-15 10:30:00 UTC
    const BASE: i64 = 1_710_498_600;

    #[test]
    fn parses_a_plain_cron() {
        let spec = CronSpec::parse("30 10 * * *").unwrap();
        // Already exactly 10:30:00 -> next is tomorrow.
        let next = spec.next_after(BASE).unwrap();
        assert_eq!(next, BASE + 24 * 3600);
    }

    #[test]
    fn parses_steps_ranges_and_lists() {
        assert!(CronSpec::parse("*/15 * * * *").is_ok());
        assert!(CronSpec::parse("0 9-17 * * *").is_ok());
        assert!(CronSpec::parse("0,30 * * * *").is_ok());
        assert!(CronSpec::parse("0 0 1 1,6,12 0").is_ok());
    }

    #[test]
    fn rejects_out_of_range_values() {
        assert!(CronSpec::parse("60 * * * *").is_err());
        assert!(CronSpec::parse("* 24 * * *").is_err());
        assert!(CronSpec::parse("* * 0 * *").is_err());
        assert!(CronSpec::parse("* * * 13 *").is_err());
        assert!(CronSpec::parse("* * * * 8").is_err());
    }

    #[test]
    fn rejects_malformed_specs_rather_than_guessing() {
        // Each of these would silently misfire under a lenient parser.
        assert!(CronSpec::parse("not a cron").is_err());
        assert!(CronSpec::parse("* * * *").is_err(), "4 fields");
        assert!(CronSpec::parse("* * * * * *").is_err(), "6 fields");
        assert!(CronSpec::parse("*/0 * * * *").is_err(), "zero step");
        assert!(CronSpec::parse("5-1 * * * *").is_err(), "inverted range");
        assert!(CronSpec::parse("1,,2 * * * *").is_err(), "empty list entry");
    }

    #[test]
    fn an_impossible_date_terminates_instead_of_hanging() {
        // February 30th never occurs. An unbounded search would spin forever.
        let spec = CronSpec::parse("0 0 30 2 *").unwrap();
        assert_eq!(spec.next_after(BASE), None);
    }

    #[test]
    fn every_fifteen_minutes() {
        let spec = CronSpec::parse("*/15 * * * *").unwrap();
        let next = spec.next_after(BASE).unwrap();
        // BASE is 10:30:00 exactly, so the next slot is 10:45.
        assert_eq!((next - BASE) / 60, 15);
    }

    #[test]
    fn a_disabled_schedule_never_fires() {
        let s = Schedule {
            id: "s".into(),
            workspace_id: "w".into(),
            name: "n".into(),
            prompt: "p".into(),
            trigger: Trigger::Cron("* * * * *".into()),
            enabled: false,
            last_run: None,
            last_status: RunStatus::Pending,
            last_run_was_read_only: false,
            created_at: BASE,
        };
        assert_eq!(s.next_fire_after(BASE), None);
    }

    #[test]
    fn a_one_shot_fires_once_then_not_again() {
        let s = Schedule {
            id: "s".into(),
            workspace_id: "w".into(),
            name: "n".into(),
            prompt: "p".into(),
            trigger: Trigger::At(BASE + 60),
            enabled: true,
            last_run: None,
            last_status: RunStatus::Pending,
            last_run_was_read_only: false,
            created_at: BASE,
        };
        assert_eq!(s.next_fire_after(BASE), Some(BASE + 60));
        // After it has run, the next firing is past -- no backfill.
        assert_eq!(s.next_fire_after(BASE + 120), None);
    }

    #[test]
    fn add_rejects_an_invalid_cron_at_insert_time() {
        let s = store(&tempfile::tempdir().unwrap());
        let err = s
            .add("ws", "n", "do a thing", Trigger::Cron("not a cron".into()))
            .unwrap_err();
        assert!(err.contains("invalid cron"));
        assert!(s.list("ws").is_empty(), "nothing should have been stored");
    }

    #[test]
    fn add_requires_a_prompt() {
        let s = store(&tempfile::tempdir().unwrap());
        assert!(s.add("ws", "n", "   ", Trigger::At(1)).is_err());
    }

    #[test]
    fn schedules_are_scoped_per_workspace() {
        let s = store(&tempfile::tempdir().unwrap());
        s.add("ws_a", "a", "p", Trigger::At(BASE)).unwrap();
        s.add("ws_b", "b", "p", Trigger::At(BASE)).unwrap();
        assert_eq!(s.list("ws_a").len(), 1);
        assert_eq!(s.list("ws_a")[0].name, "a");
        assert_eq!(s.list("ws_b")[0].name, "b");
    }

    #[test]
    fn another_workspace_cannot_remove_or_toggle_a_schedule() {
        let s = store(&tempfile::tempdir().unwrap());
        let sched = s.add("ws_a", "a", "p", Trigger::At(BASE)).unwrap();
        assert!(!s.remove("ws_b", &sched.id));
        assert!(!s.set_enabled("ws_b", &sched.id, false));
        assert_eq!(s.list("ws_a").len(), 1);
        assert!(s.list("ws_a")[0].enabled);
    }

    #[test]
    fn another_workspace_cannot_read_a_schedule() {
        let s = store(&tempfile::tempdir().unwrap());
        let sched = s.add("ws_a", "a", "p", Trigger::At(BASE)).unwrap();
        assert!(s.get("ws_b", &sched.id).is_none());
        assert!(s.get("ws_a", &sched.id).is_some());
    }

    #[test]
    fn run_outcome_is_recorded() {
        let s = store(&tempfile::tempdir().unwrap());
        let sched = s.add("ws", "n", "p", Trigger::At(BASE)).unwrap();
        assert_eq!(sched.last_status, RunStatus::Pending);
        s.record_run("ws", &sched.id, RunStatus::Ok, true);
        let after = s.get("ws", &sched.id).unwrap();
        assert!(after.last_status.is_success());
        assert!(after.last_run_was_read_only);
        assert!(after.last_run.is_some());
    }

    #[test]
    fn approval_pending_is_distinct_from_ok() {
        let s = store(&tempfile::tempdir().unwrap());
        let sched = s.add("ws", "n", "p", Trigger::At(BASE)).unwrap();
        s.record_run(
            "ws",
            &sched.id,
            RunStatus::NeedsApproval("write queued".into()),
            false,
        );
        let after = s.get("ws", &sched.id).unwrap();
        assert!(!after.last_status.is_success());
        assert_eq!(after.last_status.kind(), "needs_approval");
        assert!(!after.last_run_was_read_only);
    }

    #[test]
    fn a_never_run_schedule_is_due_immediately() {
        let s = store(&tempfile::tempdir().unwrap());
        // A one-shot in the past: trivially due.
        s.add("ws", "past", "p", Trigger::At(now_secs() - 10))
            .unwrap();
        // The case that actually regressed: a cron schedule created for a time
        // later today must still fire now, not idle until its next slot.
        s.add("ws", "cron", "p", Trigger::Cron("0 0 1 1 *".into()))
            .unwrap();
        assert_eq!(s.due(now_secs()).len(), 2);
    }

    #[test]
    fn a_never_run_schedule_wakes_the_driver_immediately() {
        let s = store(&tempfile::tempdir().unwrap());
        s.add("ws", "cron", "p", Trigger::Cron("0 0 1 1 *".into()))
            .unwrap();
        assert!(
            s.next_wakeup(now_secs()).is_some_and(|t| t <= now_secs()),
            "a never-run schedule must not make the driver sleep until its next slot"
        );
    }

    #[test]
    fn due_skips_disabled_and_already_run_schedules() {
        let s = store(&tempfile::tempdir().unwrap());
        let ran = s
            .add("ws", "ran", "p", Trigger::At(now_secs() + 3600))
            .unwrap();
        s.record_run("ws", &ran.id, RunStatus::Ok, true);
        let off = s
            .add("ws", "off", "p", Trigger::At(now_secs() - 10))
            .unwrap();
        s.set_enabled("ws", &off.id, false);
        assert!(s.due(now_secs()).is_empty());
    }

    #[test]
    fn schedules_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = ScheduleStore::open(dir.path().join("schedules.json"));
            s.add(
                "ws",
                "keep",
                "the prompt",
                Trigger::Cron("0 3 * * *".into()),
            )
            .unwrap();
        }
        let reopened = ScheduleStore::open(dir.path().join("schedules.json"));
        let all = reopened.list("ws");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].prompt, "the prompt");
        assert_eq!(all[0].trigger.kind(), "cron");
    }

    #[test]
    fn summary_omits_the_prompt() {
        let s = store(&tempfile::tempdir().unwrap());
        let sched = s
            .add("ws", "n", "SECRET PROMPT TEXT", Trigger::At(BASE))
            .unwrap();
        let json = serde_json::to_string(&sched.to_summary()).unwrap();
        assert!(!json.contains("SECRET PROMPT TEXT"));
        assert!(!json.contains("prompt"));
    }

    #[test]
    fn corrupt_file_degrades_to_empty_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedules.json");
        std::fs::write(&path, "{not json").unwrap();
        let s = ScheduleStore::open(&path);
        assert!(s.list("ws").is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    }

    #[test]
    fn next_wakeup_is_the_earliest_across_schedules() {
        let s = store(&tempfile::tempdir().unwrap());
        let now = now_secs();
        s.add("ws", "late", "p", Trigger::At(now + 7200)).unwrap();
        s.add("ws", "soon", "p", Trigger::At(now + 600)).unwrap();
        let wake = s.next_wakeup(now).unwrap();
        assert!(
            (wake - (now + 600)).abs() <= 2,
            "expected the sooner schedule"
        );
    }

    #[test]
    fn run_status_labels_are_stable() {
        assert_eq!(RunStatus::Pending.kind(), "pending");
        assert_eq!(RunStatus::Ok.kind(), "ok");
        assert_eq!(RunStatus::Failed("x".into()).kind(), "failed");
        assert_eq!(
            RunStatus::NeedsApproval("x".into()).kind(),
            "needs_approval"
        );
    }
}
