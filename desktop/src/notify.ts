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
  } catch {
    // Notifications unavailable — fall back to the in-app dialog.
  }
}

/// Post a notification for an incoming approval request (with Approve/Deny
/// buttons when supported). No-op until initNotifications succeeds.
export function notifyApproval(req: ApprovalRequest): void {
  if (!ready) return;
  try {
    sendNotification({
      title: "strkd — approval needed",
      body: req.summary,
      actionTypeId: ACTION_TYPE,
    });
  } catch {
    /* ignore */
  }
}
