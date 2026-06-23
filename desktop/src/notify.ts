// Menu-bar notifications with inline Approve / Deny buttons for incoming
// approval requests. Best-effort: if notification permission is denied or the
// platform doesn't support action buttons, this silently no-ops — the in-app
// ApprovalDialog remains the reliable way to approve/deny.

import {
  isPermissionGranted,
  requestPermission,
  registerActionTypes,
  sendNotification,
  onAction,
} from "@tauri-apps/plugin-notification";
import type { ApprovalRequest } from "./api";

const ACTION_TYPE = "strkd-approval";
let ready = false;
// Approvals can arrive before async init finishes (the FIRST pairing request
// often races app startup). Buffer them so the notification isn't dropped, then
// flush once we're ready. The in-app dialog shows regardless.
const queued: ApprovalRequest[] = [];

/// A clearer banner title per request kind — pairing and top-ups are the ones
/// the user most wants to catch.
function titleFor(req: ApprovalRequest): string {
  switch (req.method) {
    case "companion_requestPairing":
      return "strkd — pairing request";
    case "companion_requestFunding":
      return "strkd — funding (top-up) request";
    default:
      return "strkd — approval needed";
  }
}

function post(req: ApprovalRequest): void {
  try {
    sendNotification({
      title: titleFor(req),
      body: req.summary,
      actionTypeId: ACTION_TYPE,
    });
  } catch {
    /* ignore */
  }
}

/// Wire up notifications. `resolve(approved)` is called when the user clicks an
/// Approve/Deny button on the banner; it acts on the current front-of-queue
/// request (prompts are shown one at a time).
export async function initNotifications(resolve: (approved: boolean) => void): Promise<void> {
  try {
    let granted = await isPermissionGranted();
    if (!granted) granted = (await requestPermission()) === "granted";
    if (!granted) return;

    await registerActionTypes([
      {
        id: ACTION_TYPE,
        actions: [
          { id: "approve", title: "Approve" },
          { id: "deny", title: "Deny", destructive: true },
        ],
      },
    ]);

    await onAction((notification) => {
      // actionId is the button pressed.
      const actionId = (notification as { actionId?: string }).actionId;
      if (actionId === "approve") resolve(true);
      else if (actionId === "deny") resolve(false);
    });

    ready = true;
    // Replay anything that arrived during init (e.g. the first pairing prompt).
    for (const req of queued.splice(0)) post(req);
  } catch {
    // Notifications unavailable — fall back to the in-app dialog.
  }
}

/// Post a notification for an incoming approval request (with Approve/Deny
/// buttons when supported). If init hasn't completed yet, buffer it rather than
/// drop it.
export function notifyApproval(req: ApprovalRequest): void {
  if (!ready) {
    queued.push(req);
    return;
  }
  post(req);
}
