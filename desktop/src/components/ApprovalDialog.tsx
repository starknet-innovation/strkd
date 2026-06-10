import { type ApprovalRequest } from "../api";

export function ApprovalDialog({
  req,
  onApprove,
  onReject,
}: {
  req: ApprovalRequest;
  onApprove: () => void;
  onReject: () => void;
}) {
  return (
    <div className="overlay">
      <div className="dialog">
        <h3>Approve request?</h3>
        <p className="dialog-method">{req.method}</p>
        <p className="muted small">from {req.client_label}</p>
        <p className="dialog-summary">{req.summary}</p>
        <div className="dialog-actions">
          <button className="ghost" onClick={onReject}>
            Reject
          </button>
          <button className="primary" onClick={onApprove}>
            Approve
          </button>
        </div>
      </div>
    </div>
  );
}
