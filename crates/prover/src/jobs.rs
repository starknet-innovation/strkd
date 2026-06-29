//! In-memory job + activity store, generic over payload. Backs the async prove
//! flow and the UI's live activity feed. Mock and real backends feed the same
//! store, so the UI is identical before and after a real backend is plugged in.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;

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
}

const ACTIVITY_CAP: usize = 100;

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
    activity: VecDeque<Activity>,
}

pub struct Jobs {
    inner: RwLock<Inner>,
    seq: AtomicU64,
}

impl Jobs {
    /// `start_seq` seeds the job-id counter (e.g. from existing storage) so ids
    /// don't collide with prior records across restarts.
    pub fn new(start_seq: u64) -> Self {
        Jobs { inner: RwLock::new(Inner::default()), seq: AtomicU64::new(start_seq) }
    }

    /// Create a job in `Queued` and return its id (e.g. `p0`, `p1`).
    pub async fn create(&self, label: Option<String>) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("p{n}");
        let job = Job {
            job_id: job_id.clone(),
            status: JobStatus::Queued,
            started_at_ms: now_ms(),
            label,
            result: None,
            error: None,
        };
        let mut g = self.inner.write().await;
        g.jobs.insert(job_id.clone(), job.clone());
        push_activity(&mut g, &job);
        job_id
    }

    pub async fn set_status(&self, job_id: &str, status: JobStatus) {
        let mut g = self.inner.write().await;
        if let Some(j) = g.jobs.get_mut(job_id) {
            j.status = status;
            let snap = j.clone();
            push_activity(&mut g, &snap);
        }
    }

    pub async fn succeed(&self, job_id: &str, result: Value) {
        let mut g = self.inner.write().await;
        if let Some(j) = g.jobs.get_mut(job_id) {
            j.status = JobStatus::Succeeded;
            j.result = Some(result);
            let snap = j.clone();
            push_activity(&mut g, &snap);
        }
    }

    pub async fn fail(&self, job_id: &str, error: String) {
        let mut g = self.inner.write().await;
        if let Some(j) = g.jobs.get_mut(job_id) {
            j.status = JobStatus::Failed;
            j.error = Some(error);
            let snap = j.clone();
            push_activity(&mut g, &snap);
        }
    }

    pub async fn get(&self, job_id: &str) -> Option<Job> {
        self.inner.read().await.jobs.get(job_id).cloned()
    }

    /// Most-recent-first activity for the UI feed.
    pub async fn recent_activity(&self) -> Vec<Activity> {
        let g = self.inner.read().await;
        g.activity.iter().rev().cloned().collect()
    }
}

fn push_activity(inner: &mut Inner, job: &Job) {
    let seq = inner.activity.back().map(|a| a.seq + 1).unwrap_or(0);
    inner.activity.push_back(Activity {
        seq,
        job_id: job.job_id.clone(),
        status: job.status,
        started_at_ms: job.started_at_ms,
        label: job.label.clone(),
    });
    while inner.activity.len() > ACTIVITY_CAP {
        inner.activity.pop_front();
    }
}
