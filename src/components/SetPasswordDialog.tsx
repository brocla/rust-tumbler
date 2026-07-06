import { useState } from "react";

interface SetPasswordDialogProps {
  fileName: string;
  /** True when the document already has a password (the dialog changes it). */
  changing: boolean;
  onSubmit: (password: string) => void;
  onCancel: () => void;
}

/**
 * Dialog for protecting a document with a password, or changing an existing
 * one (issue #58). Confirmation field guards against a typo in the masked
 * input — a mistyped password would lock the user out of their own file after
 * Save. In-app modal reusing the PasswordPrompt styling.
 */
export function SetPasswordDialog({
  fileName,
  changing,
  onSubmit,
  onCancel,
}: SetPasswordDialogProps) {
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");

  const mismatch = confirm.length > 0 && confirm !== password;
  const canSubmit = password.length > 0 && confirm === password;

  return (
    <div className="print-progress-overlay">
      <div className="print-progress-dialog password-dialog">
        <p>
          {changing
            ? `Change the password for "${fileName}". The next Save encrypts the file with the new password.`
            : `Set a password for "${fileName}". The next Save writes an encrypted file that requires it to open.`}
        </p>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (canSubmit) onSubmit(password);
          }}
        >
          <input
            type="password"
            autoFocus
            aria-label="Password"
            placeholder="Password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          <input
            type="password"
            aria-label="Confirm password"
            placeholder="Confirm password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
          />
          {mismatch && (
            <p className="password-dialog-error">The passwords don't match.</p>
          )}
          <div className="password-dialog-buttons">
            <button type="submit" disabled={!canSubmit}>
              {changing ? "Change Password" : "Set Password"}
            </button>
            <button type="button" onClick={onCancel}>
              Cancel
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
