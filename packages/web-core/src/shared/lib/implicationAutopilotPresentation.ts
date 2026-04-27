import type {
  ImplicationAutopilotNextAction,
  ImplicationAutopilotProcessSummary,
  ImplicationAutopilotStatus,
} from './api';

interface NextActionDisplay {
  label: string;
  description: string;
}

export type ImplicationAutopilotStepState =
  | 'completed'
  | 'running'
  | 'blocked'
  | 'available'
  | 'not_started';

export interface ImplicationAutopilotStepSummary {
  label: string;
  state: ImplicationAutopilotStepState;
  summary: string;
  sessionName?: string | null;
  processId?: string | null;
  processStatus?: string | null;
}

export interface ImplicationAutopilotPanelStatus {
  implementationState: string;
  autoReviewState: string;
  latestReviewDecision: string;
  latestReviewExcerpt?: string | null;
  reviewFixState: string;
  prMergeState: string;
  nextAction: ImplicationAutopilotNextAction;
  nextActionLabel: string;
  nextActionDescription: string;
  currentStepLabel: string;
  blocker?: string | null;
  steps: ImplicationAutopilotStepSummary[];
  tokenSafetyState: 'idle' | 'guarded' | 'blocked';
  tokenSafetyNote: string;
  defaultModel: string;
  defaultReasoning: string;
  daemonized: boolean;
  workflowState: string;
  workflowStateReason: string;
  duplicatePreventionKey: string;
}

const NEXT_ACTION_DISPLAY: Record<
  ImplicationAutopilotNextAction,
  NextActionDisplay
> = {
  no_workspace: {
    label: 'No workspace',
    description: 'Link or create a local workspace before autopilot can run.',
  },
  wait_for_implementation: {
    label: 'Implementation running',
    description: 'Wait for the current implementation session to finish.',
  },
  start_auto_review: {
    label: 'Start auto-review',
    description:
      'Implementation is complete. Start one guarded Codex review for the current workspace state.',
  },
  wait_for_auto_review: {
    label: 'Review running',
    description: 'A Codex review session is checking the implementation.',
  },
  start_review_fix: {
    label: 'Start review fix',
    description: 'Auto-review requested changes; start a Codex fix session.',
  },
  wait_for_review_fix: {
    label: 'Fix running',
    description: 'Requested changes are being handled by a Codex fix session.',
  },
  ready_for_merge: {
    label: 'Ready for merge',
    description:
      'Auto-review passed; open the PR, verify checks and mergeability, then complete the handoff.',
  },
  merge_wait: {
    label: 'Merge in progress',
    description: 'Merge work has started and is still in progress.',
  },
  done: {
    label: 'Done',
    description: 'No further operator action is needed.',
  },
  investigate_failure: {
    label: 'Investigate failure',
    description: 'A session failed or produced no usable review decision.',
  },
};

export function formatImplicationAutopilotValue(value?: string | null): string {
  if (!value) {
    return 'Unknown';
  }

  return value
    .split('_')
    .filter(Boolean)
    .map((part, index) =>
      index === 0 ? part.charAt(0).toUpperCase() + part.slice(1) : part
    )
    .join(' ');
}

export function getImplicationAutopilotNextActionDisplay(
  action: ImplicationAutopilotNextAction
): NextActionDisplay {
  return NEXT_ACTION_DISPLAY[action];
}

export function buildImplicationAutopilotPanelStatus(
  status: ImplicationAutopilotStatus | null
): ImplicationAutopilotPanelStatus {
  if (!status) {
    const nextActionDisplay =
      getImplicationAutopilotNextActionDisplay('no_workspace');
    return {
      implementationState: 'Missing',
      autoReviewState: 'Missing',
      latestReviewDecision: 'Missing',
      reviewFixState: 'Not started',
      prMergeState: 'No workspace',
      nextAction: 'no_workspace',
      nextActionLabel: nextActionDisplay.label,
      nextActionDescription: nextActionDisplay.description,
      currentStepLabel: 'Implementation',
      blocker: 'No local workspace is linked to this Implication issue yet.',
      steps: [
        {
          label: 'Implementation',
          state: 'blocked',
          summary: 'Link or create a local workspace before autopilot can run.',
        },
        {
          label: 'Auto-review',
          state: 'not_started',
          summary: 'Waiting for a workspace and implementation session.',
        },
        {
          label: 'Review fix',
          state: 'not_started',
          summary: 'Only available after auto-review requests changes.',
        },
        {
          label: 'Re-review',
          state: 'not_started',
          summary: 'Only available after a review-fix session completes.',
        },
        {
          label: 'PR/checks/merge',
          state: 'not_started',
          summary: 'Manual PR handoff happens after auto-review passes.',
        },
        {
          label: 'Done/blocker',
          state: 'not_started',
          summary: 'No final outcome has been reached.',
        },
      ],
      tokenSafetyState: 'idle',
      tokenSafetyNote:
        'No Codex session is running from this panel because there is no linked workspace.',
      defaultModel: 'gpt-5.5',
      defaultReasoning: 'medium',
      daemonized: false,
      workflowState: 'Queued',
      workflowStateReason:
        'Queued until a local workspace is linked to the issue.',
      duplicatePreventionKey: 'no-workspace',
    };
  }

  const nextActionDisplay = getImplicationAutopilotNextActionDisplay(
    status.next_action
  );
  const latestReviewDecision = formatImplicationAutopilotValue(
    status.latest_review_decision
  );
  const reviewFixState = formatImplicationAutopilotValue(
    status.review_fix_state
  );
  const implementationState = formatImplicationAutopilotValue(
    status.implementation_state
  );
  const autoReviewState = formatImplicationAutopilotValue(
    status.auto_review_state
  );
  const prMergeState = formatImplicationAutopilotValue(status.pr_merge_state);
  const workflowState = formatImplicationAutopilotValue(status.workflow_state);
  const hasCompletedReviewFix = status.review_fix_state === 'completed';
  const currentStepLabel = currentStepLabelFor(status.next_action, status);
  const nextActionDescription =
    status.next_action === 'start_auto_review' && hasCompletedReviewFix
      ? 'Review fix completed; start an explicit rerun with guarded re-review for the updated workspace state.'
      : nextActionDisplay.description;

  return {
    implementationState,
    autoReviewState,
    latestReviewDecision,
    latestReviewExcerpt: status.latest_review_excerpt,
    reviewFixState,
    prMergeState,
    nextAction: status.next_action,
    nextActionLabel: nextActionDisplay.label,
    nextActionDescription,
    currentStepLabel,
    blocker: status.blocker,
    steps: buildSteps(status),
    tokenSafetyState: status.token_safety_state,
    tokenSafetyNote: status.token_safety_note,
    defaultModel: status.default_model,
    defaultReasoning: status.default_reasoning,
    daemonized: status.daemonized,
    workflowState,
    workflowStateReason: status.workflow_state_reason,
    duplicatePreventionKey: status.duplicate_prevention_key,
  };
}

function buildSteps(
  status: ImplicationAutopilotStatus
): ImplicationAutopilotStepSummary[] {
  const implementation = implementationStep(status);
  const autoReview = autoReviewStep(status);
  const reviewFix = reviewFixStep(status);
  const reReview = reReviewStep(status);
  const merge = mergeStep(status);
  const done = doneStep(status);

  return [implementation, autoReview, reviewFix, reReview, merge, done];
}

function implementationStep(
  status: ImplicationAutopilotStatus
): ImplicationAutopilotStepSummary {
  const process = status.implementation_process;
  const state = processState(status.implementation_state, process);
  return withProcess(
    {
      label: 'Implementation',
      state,
      summary:
        state === 'running'
          ? 'Implementation session is still doing work.'
          : state === 'completed'
            ? 'Implementation session finished cleanly.'
            : state === 'blocked'
              ? 'Implementation did not finish cleanly; investigate before review.'
              : 'No implementation session is attached to this workspace.',
    },
    process
  );
}

function autoReviewStep(
  status: ImplicationAutopilotStatus
): ImplicationAutopilotStepSummary {
  const process = status.auto_review_process;
  const decision = status.latest_review_decision;
  const state =
    status.next_action === 'start_auto_review' && decision === 'missing'
      ? 'available'
      : status.auto_review_state === 'running' ||
          status.next_action === 'wait_for_auto_review'
        ? 'running'
        : decision === 'pass'
          ? 'completed'
          : decision === 'request_changes'
            ? 'blocked'
            : decision === 'failed'
              ? 'blocked'
              : 'not_started';

  return withProcess(
    {
      label: 'Auto-review',
      state,
      summary:
        decision === 'request_changes'
          ? 'Latest review requested changes; tokens are idle until a fix session starts.'
          : decision === 'pass'
            ? 'Latest review passed.'
            : decision === 'failed'
              ? 'Latest review finished without a usable decision.'
              : state === 'running'
                ? 'Codex is reviewing the implementation now.'
                : state === 'available'
                  ? 'Ready to start the first guarded review session.'
                  : 'Waiting for implementation to finish.',
    },
    process
  );
}

function reviewFixStep(
  status: ImplicationAutopilotStatus
): ImplicationAutopilotStepSummary {
  const process = status.review_fix_process;
  const state =
    status.next_action === 'start_review_fix'
      ? 'available'
      : status.review_fix_state === 'running' ||
          status.next_action === 'wait_for_review_fix'
        ? 'running'
        : status.review_fix_state === 'completed'
          ? 'completed'
          : status.review_fix_state === 'failed'
            ? 'blocked'
            : 'not_started';

  return withProcess(
    {
      label: 'Review fix',
      state,
      summary:
        state === 'available'
          ? 'Start a fix session for the latest requested changes.'
          : state === 'running'
            ? 'Fix session is handling review blockers now.'
            : state === 'completed'
              ? 'Fix session completed; re-review is the next guarded step.'
              : state === 'blocked'
                ? 'Fix session failed; investigate before re-review.'
                : 'Only available after auto-review requests changes.',
    },
    process
  );
}

function reReviewStep(
  status: ImplicationAutopilotStatus
): ImplicationAutopilotStepSummary {
  const hasCompletedReviewFix = status.review_fix_state === 'completed';
  const state =
    status.next_action === 'start_auto_review' && hasCompletedReviewFix
      ? 'available'
      : status.next_action === 'wait_for_auto_review' && hasCompletedReviewFix
        ? 'running'
        : status.latest_review_decision === 'pass'
          ? 'completed'
          : 'not_started';

  return {
    label: 'Re-review',
    state,
    summary:
      state === 'available'
        ? 'Run an explicit re-review after the fix session completes.'
        : state === 'running'
          ? 'Codex is re-reviewing the fixed workspace now.'
          : state === 'completed'
            ? 'Post-fix review has passed or was not needed.'
            : 'Runs only after the fix session completes.',
  };
}

function mergeStep(
  status: ImplicationAutopilotStatus
): ImplicationAutopilotStepSummary {
  const state =
    status.next_action === 'ready_for_merge'
      ? 'available'
      : status.next_action === 'merge_wait'
        ? 'running'
        : status.pr_merge_state === 'done_or_archived'
          ? 'completed'
          : status.latest_review_decision === 'request_changes' ||
              status.latest_review_decision === 'failed'
            ? 'blocked'
            : 'not_started';

  return {
    label: 'PR/checks/merge',
    state,
    summary:
      state === 'available'
        ? 'Review passed; verify PR checks and mergeability before merge.'
        : state === 'running'
          ? 'Merge handoff is in progress.'
          : state === 'completed'
            ? 'Workspace is done or archived.'
            : state === 'blocked'
              ? 'Blocked until review passes.'
              : 'Waiting for a passing auto-review.',
  };
}

function doneStep(
  status: ImplicationAutopilotStatus
): ImplicationAutopilotStepSummary {
  const state =
    status.next_action === 'done'
      ? 'completed'
      : status.next_action === 'investigate_failure'
        ? 'blocked'
        : 'not_started';

  return {
    label: 'Done/blocker',
    state,
    summary:
      state === 'completed'
        ? 'No further operator action is needed.'
        : state === 'blocked'
          ? (status.blocker ?? 'Investigate the blocked autopilot state.')
          : 'Final outcome has not been reached yet.',
  };
}

function currentStepLabelFor(
  action: ImplicationAutopilotNextAction,
  status: ImplicationAutopilotStatus
): string {
  if (
    action === 'start_auto_review' &&
    status.review_fix_state === 'completed'
  ) {
    return 'Re-review';
  }

  switch (action) {
    case 'no_workspace':
    case 'wait_for_implementation':
    case 'start_auto_review':
      return action === 'start_auto_review' ? 'Auto-review' : 'Implementation';
    case 'wait_for_auto_review':
      return status.review_fix_state === 'completed'
        ? 'Re-review'
        : 'Auto-review';
    case 'start_review_fix':
    case 'wait_for_review_fix':
      return 'Review fix';
    case 'ready_for_merge':
    case 'merge_wait':
      return 'PR/checks/merge';
    case 'done':
    case 'investigate_failure':
      return 'Done/blocker';
  }
}

function processState(
  rawState: string,
  process?: ImplicationAutopilotProcessSummary | null
): ImplicationAutopilotStepState {
  if (process?.status === 'running' || rawState === 'running') {
    return 'running';
  }
  if (rawState === 'completed') {
    return 'completed';
  }
  if (rawState === 'failed' || process?.status === 'failed') {
    return 'blocked';
  }
  return 'not_started';
}

function withProcess(
  step: ImplicationAutopilotStepSummary,
  process?: ImplicationAutopilotProcessSummary | null
): ImplicationAutopilotStepSummary {
  if (!process) {
    return step;
  }

  return {
    ...step,
    sessionName: process.session_name,
    processId: process.id,
    processStatus: process.status,
  };
}
