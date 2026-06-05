// Compact relative time for activity/metadata, e.g. "just now", "3m ago",
// "2h ago", "4d ago". Foundation owns this; pages import it. Takes an ISO
// timestamp (Session.last_activity_at, WeaverEvent.created_at) and an optional
// reference "now" (for tests). Returns '' for empty/invalid input.
export function timeAgo(iso: string | null | undefined, now: number = Date.now()): string {
  if (!iso) return '';
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return '';
  const secs = Math.max(0, Math.round((now - then) / 1000));
  if (secs < 45) return 'just now';
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.round(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.round(hrs / 24);
  if (days < 7) return `${days}d ago`;
  const wks = Math.round(days / 7);
  if (wks < 5) return `${wks}w ago`;
  return `${Math.round(days / 30)}mo ago`;
}
