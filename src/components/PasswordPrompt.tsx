import { useState } from "react";
import { usePdfStore } from "../store/usePdfStore";

/**
 * Password prompt shown when opening a user-password-protected PDF (issue #12).
 * Driven by the store's `passwordPrompt` slice: the open flow calls
 * `askPassword` and awaits the promise, which resolves with the entered
 * password or `null` on cancel. When `retry` is set (a prior password was
 * rejected) it shows a "wrong password" hint. In-app modal to match the
 * existing UnsavedChangesDialog pattern.
 */
export function PasswordPrompt() {
  const prompt = usePdfStore((s) => s.passwordPrompt);
  const resolvePassword = usePdfStore((s) => s.resolvePassword);
  const [value, setValue] = useState("");

  if (!prompt) return null;

  const submit = () => {
    // Empty input is treated as a cancel — an empty password can't unlock a
    // user-password file, so there's nothing to try.
    resolvePassword(value.length > 0 ? value : null);
    setValue("");
  };

  const cancel = () => {
    resolvePassword(null);
    setValue("");
  };

  return (
    <div className="print-progress-overlay">
      <div className="print-progress-dialog password-dialog">
        <p>
          "{prompt.fileName}" is password-protected. Enter its password to open
          it.
        </p>
        {prompt.retry && (
          <p className="password-dialog-error">Wrong password — try again.</p>
        )}
        <form
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <input
            type="password"
            autoFocus
            aria-label="Password"
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
          <div className="password-dialog-buttons">
            <button type="submit">Open</button>
            <button type="button" onClick={cancel}>
              Cancel
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
