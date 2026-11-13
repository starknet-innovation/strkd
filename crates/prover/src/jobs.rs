//! In-memory job + activity store, generic over payload. Backs the async prove
//! flow and the UI's live activity feed. Every backend feeds the same store, so
//! the UI is uniform across the native and remote provers.
//!
//! Jobs can be **owned** by a client (the paired client that queued them, see
//! [`JobOptions::client_id`]): [`Jobs::get_for`] shows a job only to its owner.
//! Jobs can also be **ephemeral** ([`JobOptions::result_ttl_ms`]): their result
//! is dropped that long after the job finishes, and [`Jobs::wipe_ephemeral`]
//! drops it at once (the wallet calls it when it locks). The status stays.
//! A running ephemeral job is also **cancelled** by the wipe: its
//! [`Jobs::cancel_signal`] fires, and the proving task drops the prover's
//! future (the native backend then kills its subprocess and removes its temp
//! files).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::Notify;

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Lifecycle of a proving job, surfaced to pollers and the UI.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Proving,
    Succeeded,
    Failed,
}

/// Why a finished job no longer carries its result.
pub const RESULT_EXPIRED: &str = "expired";
/// The wallet locked: ephemeral results are dropped (and running ephemeral
/// jobs keep no result).
pub const RESULT_WIPED_ON_LOCK: &str = "wiped_on_lock";

#[derive(Clone, Debug, Serialize)]
pub struct Job {
    pub job_id: String,
    pub status: JobStatus,
    /// Unix-ms when the job was created (≈ proving start) — for a live elapsed timer.
    pub started_at_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The opaque proof, present once `succeeded`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A stable machine-readable reason for `error` (`UPPER_SNAKE`), when the
    /// job's creator gave one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// Unix-ms the job reached `succeeded` or `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u128>,
    /// Ephemeral jobs: Unix-ms after which the result is dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_expires_at_ms: Option<u128>,
    /// Set when an ephemeral result was dropped: [`RESULT_EXPIRED`] or
    /// [`RESULT_WIPED_ON_LOCK`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_dropped: Option<&'static str>,
    /// The client that queued the job; only it can see the job through
    /// [`Jobs::get_for`]. Never serialized.
    #[serde(skip)]
    pub client_id: Option<String>,
    /// Ephemeral jobs: how long the result is kept after the job finishes.
    #[serde(skip)]
    pub result_ttl_ms: Option<u128>,
}

impl Job {
    /// Drop an ephemeral result whose time is up.
    fn expire(&mut self, now: u128) {
        if let (Some(at), Some(_)) = (self.result_expires_at_ms, &self.result) {
            if now >= at {
                self.result = None;
                self.result_dropped = Some(RESULT_EXPIRED);
            }
        }
    }
}

/// Who owns a job and how long its result lives.
#[derive(Clone, Debug, Default)]
pub struct JobOptions {
    /// The paired client that queued the job. `None`: an untagged job (the
    /// desktop's own), which [`Jobs::get_for`] shows to no client.
    pub client_id: Option<String>,
    /// Keep the result this long after the job finishes, then drop it; also
    /// dropped by [`Jobs::wipe_ephemeral`]. `None`: kept for the process's life.
    pub result_ttl_ms: Option<u128>,
}

/// A failed job's error, with an optional machine-readable reason.
#[derive(Clone, Debug)]
pub struct JobFailure {
    pub reason: Option<String>,
    pub message: String,
}

impl From<String> for JobFailure {
    fn from(message: String) -> Self {
        JobFailure { reason: None, message }
    }
}

/// One entry in the companion's activity feed (what the UI lists).
#[derive(Clone, Debug, Serialize)]
pub struct Activity {
    pub seq: u64,
    pub job_id: String,
    pub status: JobStatus,
    pub started_at_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The client that queued the job (see [`Job::client_id`]). Never serialized.
    #[serde(skip)]
    pub client_id: Option<String>,
}

const ACTIVITY_CAP: usize = 100;

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
    activity: VecDeque<Activity>,
    /// Ephemeral jobs that were running when [`Jobs::wipe_ephemeral`] ran:
    /// they finish without a result.
    wiped_running: std::collections::HashSet<String>,
    /// Cancellation signals of ephemeral jobs that have not finished yet.
    cancels: HashMap<String, Arc<Notify>>,
}

/// The job store. Its lock is a plain mutex, never held across an `.await`,
/// so [`Jobs::wipe_ephemeral`] can run from synchronous code (a wallet lock).
pub struct Jobs {
    inner: Mutex<Inner>,
    seq: AtomicU64,
}

impl Jobs {
    /// `start_seq` seeds the job-id counter (e.g. from existing storage) so ids
    /// don't collide with prior records across restarts.
    pub fn new(start_seq: u64) -> Self {
        Jobs { inner: Mutex::new(Inner::default()), seq: AtomicU64::new(start_seq) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Create an untagged job in `Queued` and return its id (e.g. `p0`, `p1`).
    pub async fn create(&self, label: Option<String>) -> String {
        self.create_with(label, JobOptions::default()).await
    }

    /// Create a job owned by `opts.client_id`, ephemeral if `opts.result_ttl_ms`.
    pub async fn create_with(&self, label: Option<String>, opts: JobOptions) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("p{n}");
        let job = Job {
            job_id: job_id.clone(),
            status: JobStatus::Queued,
            started_at_ms: now_ms(),
            label,
            result: None,
            error: None,
            error_reason: None,
            finished_at_ms: None,
            result_expires_at_ms: None,
            result_dropped: None,
            client_id: opts.client_id,
            result_ttl_ms: opts.result_ttl_ms,
        };
        let mut g = self.lock();
        let now = now_ms();
        g.jobs.values_mut().for_each(|j| j.expire(now));
        if job.result_ttl_ms.is_some() {
            g.cancels.insert(job_id.clone(), Arc::new(Notify::new()));
        }
        g.jobs.insert(job_id.clone(), job.clone());
        push_activity(&mut g, &job);
        job_id
    }

    pub async fn set_status(&self, job_id: &str, status: JobStatus) {
        let mut g = self.lock();
        if let Some(j) = g.jobs.get_mut(job_id) {
            j.status = status;
            let snap = j.clone();
            push_activity(&mut g, &snap);
        }
    }

    /// Mark the job succeeded with `result`. An ephemeral job that was wiped
    /// while it ran keeps no result: it fails with [`RESULT_WIPED_ON_LOCK`].
    pub async fn succeed(&self, job_id: &str, result: Value) {
        let mut g = self.lock();
        if g.wiped_running.remove(job_id) {
            drop(result);
            finish(
                &mut g,
                job_id,
                Err(JobFailure {
                    reason: Some("WIPED_ON_LOCK".into()),
                    message: "the wallet locked while this job ran, so its result was discarded".into(),
                }),
            );
            return;
        }
        finish(&mut g, job_id, Ok(result));
    }

    pub async fn fail(&self, job_id: &str, error: String) {
        self.fail_with(job_id, error.into()).await
    }

    /// Mark the job failed, with an optional reason.
    pub async fn fail_with(&self, job_id: &str, failure: JobFailure) {
        let mut g = self.lock();
        g.wiped_running.remove(job_id);
        finish(&mut g, job_id, Err(failure));
    }

    /// The job, whoever owns it (desktop IPC and tests). Ephemeral results past
    /// their time are dropped first.
    pub async fn get(&self, job_id: &str) -> Option<Job> {
        let mut g = self.lock();
        let j = g.jobs.get_mut(job_id)?;
        j.expire(now_ms());
        Some(j.clone())
    }

    /// The job, only if `client_id` queued it. Other clients, and untagged
    /// jobs, read as unknown.
    pub async fn get_for(&self, job_id: &str, client_id: &str) -> Option<Job> {
        let mut g = self.lock();
        let j = g.jobs.get_mut(job_id).filter(|j| j.client_id.as_deref() == Some(client_id))?;
        j.expire(now_ms());
        Some(j.clone())
    }

    /// Whether an ephemeral job was wiped while it ran (its work should stop
    /// before doing anything irreversible).
    pub fn is_wiped(&self, job_id: &str) -> bool {
        self.lock().wiped_running.contains(job_id)
    }

    /// An ephemeral job's cancellation signal: it fires (a stored permit, so
    /// a later `notified().await` completes too) when [`Self::wipe_ephemeral`]
    /// runs while the job is unfinished. `None` for other jobs and finished ones.
    pub fn cancel_signal(&self, job_id: &str) -> Option<Arc<Notify>> {
        self.lock().cancels.get(job_id).cloned()
    }

    /// Drop every ephemeral result now, and make running ephemeral jobs finish
    /// without one: each one's [`Self::cancel_signal`] fires, so its proving
    /// task stops the prover. Synchronous: the wallet calls it from its lock path.
    pub fn wipe_ephemeral(&self) {
        let mut g = self.lock();
        let mut running = Vec::new();
        for j in g.jobs.values_mut().filter(|j| j.result_ttl_ms.is_some()) {
            match j.status {
                JobStatus::Queued | JobStatus::Proving => running.push(j.job_id.clone()),
                JobStatus::Succeeded | JobStatus::Failed => {
                    if j.result.take().is_some() {
                        j.result_dropped = Some(RESULT_WIPED_ON_LOCK);
                    }
                }
            }
        }
        for id in &running {
            if let Some(n) = g.cancels.get(id) {
                n.notify_one();
            }
        }
        g.wiped_running.extend(running);
    }

    /// Most-recent-first activity for the UI feed (every job: desktop IPC).
    pub async fn recent_activity(&self) -> Vec<Activity> {
        let g = self.lock();
        g.activity.iter().rev().cloned().collect()
    }

    /// Most-recent-first activity of the jobs `client_id` queued (JSON-RPC).
    pub async fn recent_activity_for(&self, client_id: &str) -> Vec<Activity> {
        let g = self.lock();
        g.activity.iter().rev().filter(|a| a.client_id.as_deref() == Some(client_id)).cloned().collect()
    }
}

fn finish(inner: &mut Inner, job_id: &str, outcome: Result<Value, JobFailure>) {
    inner.cancels.remove(job_id);
    let Some(j) = inner.jobs.get_mut(job_id) else { return };
    let now = now_ms();
    j.finished_at_ms = Some(now);
    match outcome {
        Ok(result) => {
            j.status = JobStatus::Succeeded;
            j.result = Some(result);
            j.result_expires_at_ms = j.result_ttl_ms.map(|ttl| now + ttl);
        }
        Err(f) => {
            j.status = JobStatus::Failed;
            j.error = Some(f.message);
            j.error_reason = f.reason;
        }
    }
    let snap = j.clone();
    push_activity(inner, &snap);
}

fn push_activity(inner: &mut Inner, job: &Job) {
    let seq = inner.activity.back().map(|a| a.seq + 1).unwrap_or(0);
    inner.activity.push_back(Activity {
        seq,
        job_id: job.job_id.clone(),
        status: job.status,
        started_at_ms: job.started_at_ms,
        label: job.label.clone(),
        client_id: job.client_id.clone(),
    });
    while inner.activity.len() > ACTIVITY_CAP {
        inner.activity.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn owned(client: &str, ttl: Option<u128>) -> JobOptions {
        JobOptions { client_id: Some(client.into()), result_ttl_ms: ttl }
    }

    #[tokio::test]
    async fn owned_jobs_are_visible_only_to_their_client() {
        let jobs = Jobs::new(0);
        let a = jobs.create_with(None, owned("c1", None)).await;
        let untagged = jobs.create(None).await;
        assert!(jobs.get_for(&a, "c1").await.is_some());
        assert!(jobs.get_for(&a, "c2").await.is_none());
        assert!(jobs.get_for(&untagged, "c1").await.is_none());
        // The desktop (and tests) still see everything.
        assert!(jobs.get(&a).await.is_some() && jobs.get(&untagged).await.is_some());
        // The owner is never serialized.
        let s = serde_json::to_string(&jobs.get(&a).await.unwrap()).unwrap();
        assert!(!s.contains("c1"), "{s}");
    }

    #[tokio::test]
    async fn ephemeral_results_expire_and_keep_their_status() {
        let jobs = Jobs::new(0);
        let id = jobs.create_with(None, owned("c", Some(30))).await;
        jobs.succeed(&id, json!({ "proof": "x" })).await;
        let j = jobs.get_for(&id, "c").await.unwrap();
        assert_eq!(j.result, Some(json!({ "proof": "x" })));
        assert!(j.result_expires_at_ms.is_some());
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        let j = jobs.get_for(&id, "c").await.unwrap();
        assert_eq!(j.status, JobStatus::Succeeded);
        assert!(j.result.is_none());
        assert_eq!(j.result_dropped, Some(RESULT_EXPIRED));
    }

    #[tokio::test]
    async fn wiping_drops_finished_results_and_running_jobs_finish_empty() {
        let jobs = Jobs::new(0);
        let done = jobs.create_with(None, owned("c", Some(3_600_000))).await;
        jobs.succeed(&done, json!(1)).await;
        let running = jobs.create_with(None, owned("c", Some(3_600_000))).await;
        jobs.set_status(&running, JobStatus::Proving).await;
        let kept = jobs.create_with(None, owned("c", None)).await;
        jobs.succeed(&kept, json!(2)).await;

        jobs.wipe_ephemeral();
        let d = jobs.get(&done).await.unwrap();
        assert!(d.result.is_none());
        assert_eq!(d.result_dropped, Some(RESULT_WIPED_ON_LOCK));
        assert!(jobs.is_wiped(&running));
        jobs.succeed(&running, json!(3)).await;
        let r = jobs.get(&running).await.unwrap();
        assert_eq!(r.status, JobStatus::Failed);
        assert!(r.result.is_none());
        assert_eq!(r.error_reason.as_deref(), Some("WIPED_ON_LOCK"));
        assert!(!jobs.is_wiped(&running));
        // Non-ephemeral results are untouched.
        assert_eq!(jobs.get(&kept).await.unwrap().result, Some(json!(2)));
    }

    #[tokio::test]
    async fn wiping_fires_the_cancel_signal_of_running_ephemeral_jobs_only() {
        let jobs = Jobs::new(0);
        let running = jobs.create_with(None, owned("c", Some(3_600_000))).await;
        let kept = jobs.create_with(None, owned("c", None)).await;
        assert!(jobs.cancel_signal(&kept).is_none());
        let signal = jobs.cancel_signal(&running).expect("ephemeral jobs have a signal");
        jobs.wipe_ephemeral();
        // The permit is stored: waiting after the wipe still completes.
        tokio::time::timeout(std::time::Duration::from_secs(1), signal.notified()).await.expect("cancelled");
        // A finished job has no signal left.
        jobs.fail(&running, "x".into()).await;
        assert!(jobs.cancel_signal(&running).is_none());
    }

    #[tokio::test]
    async fn activity_is_filtered_per_client() {
        let jobs = Jobs::new(0);
        let a = jobs.create_with(Some("A".into()), owned("c1", None)).await;
        let b = jobs.create_with(Some("B".into()), owned("c2", None)).await;
        let _untagged = jobs.create(Some("desk".into())).await;
        let c1: Vec<String> = jobs.recent_activity_for("c1").await.into_iter().map(|x| x.job_id).collect();
        assert_eq!(c1, vec![a]);
        let c2: Vec<String> = jobs.recent_activity_for("c2").await.into_iter().map(|x| x.job_id).collect();
        assert_eq!(c2, vec![b]);
        assert_eq!(jobs.recent_activity().await.len(), 3);
        let s = serde_json::to_string(&jobs.recent_activity().await).unwrap();
        assert!(!s.contains("c1") && !s.contains("client_id"), "{s}");
    }
}
