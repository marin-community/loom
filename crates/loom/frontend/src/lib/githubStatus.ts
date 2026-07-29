import type { GithubStatus } from '../types';

export interface GithubChip {
  key: string;
  label: string;
  cls: string;
}

export function githubStateChip(gh: GithubStatus): GithubChip {
  const draft = gh.is_draft && gh.pr_state === 'OPEN';
  const key = draft ? 'DRAFT' : gh.pr_state;
  const tint: Record<string, string> = {
    OPEN: 'text-ok',
    MERGED: 'text-agent',
    CLOSED: 'text-block',
    DRAFT: 'text-faint',
  };
  return { key, label: key.toLowerCase(), cls: tint[key] ?? 'text-muted' };
}

export function githubReviewChip(gh: GithubStatus): GithubChip | null {
  const review = gh.review_decision;
  if (!review) return null;
  const chips: Record<string, GithubChip> = {
    APPROVED: { key: 'APPROVED', label: 'approved', cls: 'text-ok' },
    CHANGES_REQUESTED: {
      key: 'CHANGES_REQUESTED',
      label: 'changes requested',
      cls: 'text-block',
    },
    REVIEW_REQUIRED: {
      key: 'REVIEW_REQUIRED',
      label: 'review required',
      cls: 'text-attn-line',
    },
  };
  return (
    chips[review] ?? {
      key: review,
      label: review.toLowerCase().replace(/_/g, ' '),
      cls: 'text-muted',
    }
  );
}

export function githubChecksChip(gh: GithubStatus): GithubChip | null {
  const checks = gh.checks;
  if (!checks) return null;
  const chips: Record<string, GithubChip> = {
    passing: { key: 'passing', label: 'CI passing', cls: 'text-ok' },
    failing: { key: 'failing', label: 'CI failing', cls: 'text-block' },
    pending: { key: 'pending', label: 'CI pending', cls: 'text-info' },
  };
  return chips[checks] ?? { key: checks, label: `CI ${checks}`, cls: 'text-muted' };
}

export function githubConflictChip(gh: GithubStatus): GithubChip | null {
  return gh.mergeable === 'CONFLICTING'
    ? { key: 'CONFLICTING', label: 'conflicts', cls: 'text-block' }
    : null;
}
