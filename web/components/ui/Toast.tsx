'use client';

import { useEffect } from 'react';

/// Transient bottom-right notice; auto-dismisses. Ported verbatim from Lumen.
export function Toast({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  useEffect(() => {
    const t = setTimeout(onDismiss, 3200);
    return () => clearTimeout(t);
  }, [onDismiss]);

  return <div className="toast">{message}</div>;
}
