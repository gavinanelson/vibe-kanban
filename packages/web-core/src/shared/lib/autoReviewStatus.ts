export type AutoReviewProcessState =
  | 'starting'
  | 'running'
  | 'completed'
  | 'failed';

export interface AutoReviewStatusBadge {
  state: AutoReviewProcessState;
  label: string;
}

export interface PendingAutoReview {
  state: AutoReviewProcessState;
  localWorkspaceId?: string | null;
  sessionId?: string | null;
  processId?: string | null;
  requestedAt?: string | null;
}

interface AutoReviewWorkspaceSignals {
  isRunning?: boolean;
  latestProcessCompletedAt?: string;
  latestProcessStatus?: 'running' | 'completed' | 'failed' | 'killed';
}

interface DeriveAutoReviewStatusInput {
  issueStatusName?: string | null;
  workspace?: AutoReviewWorkspaceSignals | null;
  pendingReview?: PendingAutoReview | null;
}

const REVIEW_STATUS_NAME_PATTERN = /\breview\b/i;

const LABEL_BY_STATE: Record<AutoReviewProcessState, string> = {
  starting: 'Review starting',
  running: 'Review running',
  completed: 'Review complete',
  failed: 'Review failed',
};

function isAfterOrEqual(left?: string | null, right?: string | null): boolean {
  if (!left || !right) {
    return false;
  }

  const leftTime = Date.parse(left);
  const rightTime = Date.parse(right);
  if (!Number.isFinite(leftTime) || !Number.isFinite(rightTime)) {
    return false;
  }

  return leftTime >= rightTime;
}

function badge(state: AutoReviewProcessState): AutoReviewStatusBadge {
  return {
    state,
    label: LABEL_BY_STATE[state],
  };
}

export function isReviewStatusName(statusName?: string | null): boolean {
  return REVIEW_STATUS_NAME_PATTERN.test(statusName ?? '');
}

export function deriveAutoReviewStatus({
  issueStatusName,
  workspace,
  pendingReview,
}: DeriveAutoReviewStatusInput): AutoReviewStatusBadge | null {
  if (pendingReview?.state === 'starting') {
    return badge('starting');
  }

  const completedAfterRequest = isAfterOrEqual(
    workspace?.latestProcessCompletedAt,
    pendingReview?.requestedAt
  );

  if (
    pendingReview &&
    completedAfterRequest &&
    workspace?.latestProcessStatus === 'completed'
  ) {
    return badge('completed');
  }

  if (
    pendingReview &&
    completedAfterRequest &&
    (workspace?.latestProcessStatus === 'failed' ||
      workspace?.latestProcessStatus === 'killed')
  ) {
    return badge('failed');
  }

  if (pendingReview?.state === 'failed') {
    return badge('failed');
  }

  if (pendingReview) {
    return badge('running');
  }

  if (isReviewStatusName(issueStatusName) && workspace?.isRunning) {
    return badge('running');
  }

  return null;
}
