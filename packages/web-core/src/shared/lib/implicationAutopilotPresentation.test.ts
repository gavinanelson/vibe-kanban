import { describe, expect, it } from 'vitest';

import {
  buildImplicationAutopilotPanelStatus,
  formatImplicationAutopilotValue,
  getImplicationAutopilotNextActionDisplay,
} from './implicationAutopilotPresentation';
import type { ImplicationAutopilotStatus } from './api';

const baseStatus: ImplicationAutopilotStatus = {
  workspace_id: 'workspace-1',
  workspace_name: 'Issue 264',
  implementation_state: 'completed',
  auto_review_state: 'missing',
  latest_review_decision: 'missing',
  latest_review_excerpt: null,
  review_fix_state: 'not_started',
  pr_merge_state: 'waiting_for_review',
  next_action: 'start_auto_review',
  blocker: null,
  implementation_process: {
    id: 'process-implementation',
    session_id: 'session-implementation',
    session_name: 'Implementation',
    status: 'completed',
    run_reason: 'codingagent',
    exit_code: 0,
    started_at: '2026-04-25T12:00:00Z',
    completed_at: '2026-04-25T12:10:00Z',
  },
  auto_review_process: null,
  review_fix_process: null,
  default_model: 'gpt-5.5',
  default_reasoning: 'medium',
  daemonized: false,
  token_safety_state: 'guarded',
  token_safety_note:
    'Auto-review reruns are guarded: the server only exposes review starts when no agent is running and reruns are explicit after a completed review-fix.',
};

describe('implication autopilot presentation', () => {
  it('formats next action tokens as operator-facing copy', () => {
    expect(
      getImplicationAutopilotNextActionDisplay('start_auto_review')
    ).toEqual({
      label: 'Start auto-review',
      description:
        'Implementation is complete. Start one guarded Codex review for the current workspace state.',
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
          'Auto-review passed; open the PR, verify checks and mergeability, then complete the handoff.',
      }
    );
  });

  it('keeps unknown status values readable without losing the raw signal', () => {
    expect(formatImplicationAutopilotValue('blocked_by_review')).toBe(
      'Blocked by review'
    );
  });

  it.each([
    ['start_auto_review', 'Auto-review', 'available'],
    ['wait_for_auto_review', 'Auto-review', 'running'],
    ['start_review_fix', 'Review fix', 'available'],
    ['wait_for_review_fix', 'Review fix', 'running'],
    ['ready_for_merge', 'PR/checks/merge', 'available'],
    ['investigate_failure', 'Done/blocker', 'blocked'],
  ] as const)(
    'marks the current timeline step for %s',
    (nextAction, label, state) => {
      const panel = buildImplicationAutopilotPanelStatus({
        ...baseStatus,
        next_action: nextAction,
      });

      expect(panel.currentStepLabel).toBe(label);
      expect(panel.steps.find((step) => step.label === label)?.state).toBe(
        state
      );
    }
  );

  it('shows request-changes review fix and explicit re-review copy', () => {
    const panel = buildImplicationAutopilotPanelStatus({
      ...baseStatus,
      auto_review_state: 'request_changes',
      latest_review_decision: 'request_changes',
      latest_review_excerpt: 'Decision: request changes. Blocker: tests fail.',
      review_fix_state: 'not_started',
      pr_merge_state: 'blocked_by_review',
      next_action: 'start_review_fix',
    });

    expect(panel.steps.map((step) => [step.label, step.state])).toEqual([
      ['Implementation', 'completed'],
      ['Auto-review', 'blocked'],
      ['Review fix', 'available'],
      ['Re-review', 'not_started'],
      ['PR/checks/merge', 'not_started'],
      ['Done/blocker', 'not_started'],
    ]);
    expect(panel.latestReviewExcerpt).toContain('Decision: request changes');
    expect(
      panel.steps.find((step) => step.label === 'Re-review')?.summary
    ).toContain('after the fix session completes');
  });

  it('turns a completed review fix into guarded re-review copy', () => {
    const panel = buildImplicationAutopilotPanelStatus({
      ...baseStatus,
      auto_review_state: 'request_changes',
      latest_review_decision: 'request_changes',
      review_fix_state: 'completed',
      pr_merge_state: 'blocked_by_review',
      next_action: 'start_auto_review',
      blocker: 'Review fix completed; rerun auto-review.',
      review_fix_process: {
        id: 'process-review-fix',
        session_id: 'session-review-fix',
        session_name: 'Review fix - Codex (medium)',
        status: 'completed',
        run_reason: 'codingagent',
        exit_code: 0,
        started_at: '2026-04-25T12:20:00Z',
        completed_at: '2026-04-25T12:30:00Z',
      },
    });

    expect(panel.currentStepLabel).toBe('Re-review');
    expect(panel.steps.find((step) => step.label === 'Re-review')?.state).toBe(
      'available'
    );
    expect(panel.nextActionDescription).toContain('explicit rerun');
    expect(panel.tokenSafetyNote).toContain('guarded');
  });

  it('returns no-workspace blocker copy without implying active token use', () => {
    const panel = buildImplicationAutopilotPanelStatus(null);

    expect(panel.currentStepLabel).toBe('Implementation');
    expect(panel.blocker).toContain('No local workspace');
    expect(panel.steps[0]).toMatchObject({
      label: 'Implementation',
      state: 'blocked',
    });
    expect(panel.tokenSafetyNote).toContain('No Codex session is running');
  });
});
