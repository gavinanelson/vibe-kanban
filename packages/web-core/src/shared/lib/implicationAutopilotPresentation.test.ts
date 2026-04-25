import { describe, expect, it } from 'vitest';

import {
  formatImplicationAutopilotValue,
  getImplicationAutopilotNextActionDisplay,
} from './implicationAutopilotPresentation';

describe('implication autopilot presentation', () => {
  it('formats next action tokens as operator-facing copy', () => {
    expect(
      getImplicationAutopilotNextActionDisplay('start_auto_review')
    ).toEqual({
      label: 'Start auto-review',
      description: 'Implementation is complete and ready for a Codex review.',
    });

    expect(
      getImplicationAutopilotNextActionDisplay('wait_for_review_fix')
    ).toEqual({
      label: 'Fix running',
      description:
        'Requested changes are being handled by a Codex fix session.',
    });

    expect(getImplicationAutopilotNextActionDisplay('ready_for_merge')).toEqual(
      {
        label: 'Ready for merge',
        description:
          'Auto-review passed; open the PR and complete the merge handoff.',
      }
    );
  });

  it('keeps unknown status values readable without losing the raw signal', () => {
    expect(formatImplicationAutopilotValue('blocked_by_review')).toBe(
      'Blocked by review'
    );
  });
});
