const ENCODER = new TextEncoder();

/** Length of `value` in UTF-8 bytes. */
export function utf8Length(value: string): number {
  return ENCODER.encode(value).length;
}

const ABSOLUTE = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const RELATIVE = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

/** Formats an RFC 3339 timestamp as an absolute local date and time, or "unknown". */
export function formatAbsolute(iso: string | null): string {
  if (iso === null) {
    return "unknown";
  }
  const time = Date.parse(iso);
  return Number.isNaN(time) ? "unknown" : ABSOLUTE.format(time);
}

/** Formats a timestamp relative to `now` within a day, otherwise as an absolute date. */
export function formatUpdated(iso: string | null, now: number = Date.now()): string {
  if (iso === null) {
    return "unknown";
  }
  const time = Date.parse(iso);
  if (Number.isNaN(time)) {
    return "unknown";
  }
  const seconds = Math.round((time - now) / 1000);
  if (seconds > 0 || seconds < -86_400) {
    return ABSOLUTE.format(time);
  }
  if (seconds > -60) {
    return RELATIVE.format(0, "second");
  }
  if (seconds > -3600) {
    return RELATIVE.format(Math.round(seconds / 60), "minute");
  }
  return RELATIVE.format(Math.round(seconds / 3600), "hour");
}

const SHORT_DATE = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
const SHORT_DATE_YEAR = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  year: "numeric",
});

/**
 * Formats a timestamp as a compact age against `now` (milliseconds): "now", "5m", "3h", "2d",
 * "4w", then a short date. A time in the future reads "now"; a missing or invalid one "unknown".
 */
export function formatTimeAgo(iso: string | null, now: number): string {
  if (iso === null) {
    return "unknown";
  }
  const time = Date.parse(iso);
  if (Number.isNaN(time)) {
    return "unknown";
  }
  const seconds = Math.floor((now - time) / 1000);
  if (seconds < 60) {
    return "now";
  }
  if (seconds < 3600) {
    return `${Math.floor(seconds / 60)}m`;
  }
  if (seconds < 86_400) {
    return `${Math.floor(seconds / 3600)}h`;
  }
  if (seconds < 7 * 86_400) {
    return `${Math.floor(seconds / 86_400)}d`;
  }
  if (seconds < 35 * 86_400) {
    return `${Math.floor(seconds / (7 * 86_400))}w`;
  }
  const sameYear = new Date(time).getFullYear() === new Date(now).getFullYear();
  return (sameYear ? SHORT_DATE : SHORT_DATE_YEAR).format(time);
}

/** Formats whole seconds as mm:ss. Negative values show 00:00. */
export function formatCountdown(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(total / 60);
  const rest = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
}

/** Formats a Unix timestamp in seconds as an absolute local date and time. */
export function formatUnix(seconds: number): string {
  return ABSOLUTE.format(seconds * 1000);
}
