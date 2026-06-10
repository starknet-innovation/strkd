//! The approval broker.
//!
//! Any signing or state-changing method must obtain user approval before it
//! proceeds. `Approver` is the seam that makes this testable: the real
//! implementation pushes a prompt to the menu-bar UI and blocks on the user's
//! decision; tests inject a deterministic approver.
//!
//! Crucially, an `ApprovalRequest` carries only **public** information (caller,
//! method, a human summary) — never key material — so it is safe to surface.

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

/// What the user is being asked to approve.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub client_label: String,
    pub method: String,
    pub summary: String,
}

/// The user's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn request_approval(&self, req: ApprovalRequest) -> Decision;
}

/// Test/headless approver that always returns a fixed decision.
pub struct AutoApprover(pub Decision);

#[async_trait]
impl Approver for AutoApprover {
    async fn request_approval(&self, _req: ApprovalRequest) -> Decision {
        self.0
    }
}

/// A pending prompt handed to the UI: the request plus a channel to answer on.
pub struct PendingApproval {
    pub request: ApprovalRequest,
    pub respond: oneshot::Sender<Decision>,
}

/// Channel-backed approver for the real (Tauri) UI.
///
/// `request_approval` forwards the prompt over an mpsc channel and awaits the
/// reply. The UI side consumes [`PendingApproval`]s from the paired receiver,
/// shows the menu-bar dialog, and sends back the [`Decision`]. If the receiver
/// is gone (UI shut down), the request is treated as a rejection.
pub struct ChannelApprover {
    tx: mpsc::Sender<PendingApproval>,
}

impl ChannelApprover {
    /// Create an approver and the receiver the UI loop should drain.
    pub fn new(buffer: usize) -> (Self, mpsc::Receiver<PendingApproval>) {
        let (tx, rx) = mpsc::channel(buffer);
        (ChannelApprover { tx }, rx)
    }
}

#[async_trait]
impl Approver for ChannelApprover {
    async fn request_approval(&self, req: ApprovalRequest) -> Decision {
        let (respond, answer) = oneshot::channel();
        let pending = PendingApproval {
            request: req,
            respond,
        };
        if self.tx.send(pending).await.is_err() {
            return Decision::Reject;
        }
        answer.await.unwrap_or(Decision::Reject)
    }
}
