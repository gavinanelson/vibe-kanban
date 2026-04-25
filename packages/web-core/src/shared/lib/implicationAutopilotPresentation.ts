import type { ImplicationAutopilotNextAction } from './api';

interface NextActionDisplay {
  label: string;
  description: string;
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
    description: 'Implementation is complete and ready for a Codex review.',
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
    description: 'Auto-review passed; open the PR and complete the merge handoff.',
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
