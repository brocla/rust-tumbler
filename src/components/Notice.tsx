import { useEffect } from "react";
import { usePdfStore } from "../store/usePdfStore";

/**
 * A transient, dismissible status toast driven by `store.notice`. Used for
 * short informational messages such as clicking a form button whose scripted
 * action Tumbler can't run. Auto-dismisses after a few seconds.
 */
export function Notice() {
  const notice = usePdfStore((s) => s.notice);
  const clearNotice = usePdfStore((s) => s.clearNotice);

  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(clearNotice, 4000);
    return () => clearTimeout(timer);
  }, [notice, clearNotice]);

  if (!notice) return null;

  return (
    <div className="notice-toast" role="status" onClick={clearNotice}>
      {notice}
    </div>
  );
}
