'use client';

/// The Quartz toggle, ported verbatim from Lumen / Quartz Command. The `.switch`
/// classes live in globals.css so a switch written in markup matches this one.
export function Switch({ on, onChange }: { on: boolean; onChange: (v: boolean) => void }) {
  return (
    <div
      role="switch"
      aria-checked={on}
      onClick={() => onChange(!on)}
      className={`switch ${on ? 'on' : ''}`}
    >
      <div className="switch-knob" />
    </div>
  );
}
