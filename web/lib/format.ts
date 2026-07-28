/**
 * Display formatting.
 *
 * Two rules run through this file, both from the domain rather than taste:
 *
 *   - Storage and memory use binary prefixes (KiB, MiB); network rates use
 *     decimal ones (Mb/s, Gb/s). A 100G NIC is 100,000,000,000 bits per second,
 *     not 2^37, and showing it as "93.1 Gb/s" would be wrong.
 *   - Timestamps arrive UTC and are rendered in the operator's local zone. A
 *     test that ran "at 14:05" means the wall clock in the lab.
 */

/** Decimal unit ladder, for bits per second. */
const RATE_UNITS = ['b/s', 'kb/s', 'Mb/s', 'Gb/s', 'Tb/s'] as const;

/** Binary unit ladder, for bytes on disk and in memory. */
const BYTE_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] as const;

/** Formats a link speed given in megabits per second. */
export function formatSpeed(mbps: number | null | undefined): string {
  if (mbps === null || mbps === undefined || mbps <= 0) return '—';
  if (mbps >= 1000) {
    const gbps = mbps / 1000;
    return `${Number.isInteger(gbps) ? gbps : gbps.toFixed(1)}G`;
  }
  return `${mbps}M`;
}

/** Formats a rate in bits per second. */
export function formatBitrate(bps: number): string {
  return scale(bps, 1000, RATE_UNITS, 2);
}

/** Formats a byte count with binary prefixes. */
export function formatBytes(bytes: number): string {
  return scale(bytes, 1024, BYTE_UNITS, 1);
}

/** Formats a packet rate. */
export function formatPps(pps: number): string {
  return scale(pps, 1000, ['pps', 'kpps', 'Mpps', 'Gpps'] as const, 2);
}

/** Walks a value up a unit ladder. */
function scale(
  value: number,
  step: number,
  units: readonly string[],
  decimals: number,
): string {
  if (!Number.isFinite(value)) return '—';

  const sign = value < 0 ? '-' : '';
  let n = Math.abs(value);
  let i = 0;
  while (n >= step && i < units.length - 1) {
    n /= step;
    i += 1;
  }

  // Whole numbers in the base unit are counts, not measurements — "512 B" reads
  // better than "512.0 B".
  const text = i === 0 && Number.isInteger(n) ? String(n) : n.toFixed(decimals);
  return `${sign}${text} ${units[i]}`;
}

/** Formats a plain integer with thousands separators. */
export function formatCount(n: number): string {
  return n.toLocaleString(undefined, { maximumFractionDigits: 0 });
}

/** Formats a percentage. */
export function formatPercent(fraction: number, decimals = 1): string {
  if (!Number.isFinite(fraction)) return '—';
  return `${(fraction * 100).toFixed(decimals)}%`;
}

/**
 * Formats a duration in seconds as the two largest meaningful units.
 *
 * Two units is the readable middle ground: "3d 4h" tells an operator what they
 * need, where "3d 4h 12m 6s" makes them parse a sentence.
 */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—';

  const s = Math.floor(seconds);
  if (s < 60) return `${s}s`;

  const parts: string[] = [];
  const days = Math.floor(s / 86400);
  const hours = Math.floor((s % 86400) / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const secs = s % 60;

  if (days) parts.push(`${days}d`);
  if (hours) parts.push(`${hours}h`);
  if (minutes && parts.length < 2) parts.push(`${minutes}m`);
  if (secs && parts.length < 2 && !days) parts.push(`${secs}s`);

  return parts.slice(0, 2).join(' ');
}

/** Formats a UTC ISO timestamp in the viewer's local zone. */
export function formatTimestamp(iso: string | null | undefined): string {
  if (!iso) return '—';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '—';
  return date.toLocaleString(undefined, {
    year: 'numeric',
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  });
}

/** Formats a timestamp as a relative age, e.g. "4m ago" or "in 3h". */
export function formatRelative(iso: string | null | undefined): string {
  if (!iso) return '—';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '—';

  const deltaSecs = (date.getTime() - Date.now()) / 1000;
  const magnitude = formatDuration(Math.abs(deltaSecs));
  if (Math.abs(deltaSecs) < 5) return 'just now';
  return deltaSecs < 0 ? `${magnitude} ago` : `in ${magnitude}`;
}
