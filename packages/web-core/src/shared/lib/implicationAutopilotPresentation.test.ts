import { deepEqual, equal, match } from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  buildImplicationAutopilotPanelStatus,
  formatImplicationAutopilotValue,
  getImplicationAutopilotNextActionDisplay,
} from './implicationAutopilotPresentation.ts';
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
  daemonized: true,
  workflow_state: 'ready_to_advance',
  workflow_state_reason:
    'Implementation completed; app advance may promote to In review and start exactly one auto-review.',
  duplicate_prevention_key:
    'workspace:implementation:no-review:no-review-fix:Missing',
  token_safety_state: 'guarded',
  token_safety_note:
    'Auto-review reruns are guarded: the server only exposes review starts when no agent is running and reruns are explicit after a completed review-fix.',
};

describe('implication autopilot presentation', () => {
  it('formats next action tokens as operator-facing copy', () => {
    deepEqual(getImplicationAutopilotNextActionDisplay('start_auto_review'), {
      label: 'Start auto-review',
      description:
        'Implementation is complete. Start one guarded Codex review for the current workspace state.',
    });

    deepEqual(getImplicationAutopilotNextActionDisplay('wait_for_review_fix'), {
      label: 'Fix running',
      description:
        'Requested changes are being handled by a Codex fix session.',
    });

    deepEqual(getImplicationAutopilotNextActionDisplay('ready_for_merge'), {
      label: 'Ready for merge',
      description:
        'Auto-review passed; open the PR, verify checks and mergeability, then complete the handoff.',
    });
  });

  it('keeps unknown status values readable without losing the raw signal', () => {
    equal(
      formatImplicationAutopilotValue('blocked_by_review'),
      'Blocked by review'
    );
  });

  for (const [nextAction, label, state] of [
    ['start_auto_review', 'Auto-review', 'available'],
    ['wait_for_auto_review', 'Auto-review', 'running'],
    ['start_review_fix', 'Review fix', 'available'],
    ['wait_for_review_fix', 'Review fix', 'running'],
    ['ready_for_merge', 'PR/checks/merge', 'available'],
    ['investigate_failure', 'Done/blocker', 'blocked'],
  ] as const) {
    it(`marks the current timeline step for ${nextAction}`, () => {
      const panel = buildImplicationAutopilotPanelStatus({
        ...baseStatus,
        next_action: nextAction,
      });

      equal(panel.currentStepLabel, label);
      equal(panel.steps.find((step) => step.label === label)?.state, state);
    });
  }

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

    deepEqual(
      panel.steps.map((step) => [step.label, step.state]),
      [
        ['Implementation', 'completed'],
        ['Auto-review', 'blocked'],
        ['Review fix', 'available'],
        ['Re-review', 'not_started'],
        ['PR/checks/merge', 'blocked'],
        ['Done/blocker', 'not_started'],
      ]
    );
    match(panel.latestReviewExcerpt ?? '', /Decision: request changes/);
    match(
      panel.steps.find((step) => step.label === 'Re-review')?.summary ?? '',
      /after the fix session completes/
    );
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

    equal(panel.currentStepLabel, 'Re-review');
    equal(
      panel.steps.find((step) => step.label === 'Re-review')?.state,
      'available'
    );
    match(panel.nextActionDescription, /explicit rerun/);
    match(panel.tokenSafetyNote, /guarded/);
  });

  it('shows merge readiness as a handoff, not an automated merge claim', () => {
    const panel = buildImplicationAutopilotPanelStatus({
      ...baseStatus,
      auto_review_state: 'pass',
      latest_review_decision: 'pass',
      pr_merge_state: 'review_passed_merge_status_unknown',
      next_action: 'ready_for_merge',
      blocker:
        'Review passed; PR/check mergeability is not daemonized in this UI slice yet.',
    });

    equal(panel.currentStepLabel, 'PR/checks/merge');
    equal(
      panel.steps.find((step) => step.label === 'PR/checks/merge')?.state,
      'available'
    );
    match(panel.nextActionDescription, /verify checks and mergeability/);
    match(panel.blocker ?? '', /not daemonized/);
  });

  it('surfaces app-owned workflow state and duplicate prevention key', () => {
    const panel = buildImplicationAutopilotPanelStatus(baseStatus);

    equal(panel.workflowState, 'Ready to advance');
    match(panel.workflowStateReason, /exactly one auto-review/);
    match(panel.duplicatePreventionKey, /implementation/);
  });

  it('surfaces dirty or conflicting PR blockers in the final blocker step', () => {
    const panel = buildImplicationAutopilotPanelStatus({
      ...baseStatus,
      latest_review_decision: 'pass',
      pr_merge_state: 'blocked_by_pr_conflict',
      next_action: 'investigate_failure',
      blocker:
        'PR is dirty or conflicting; it is not safe to re-review or merge.',
      token_safety_state: 'blocked',
      token_safety_note:
        'PR is dirty or conflicting; it is not safe to re-review or merge.',
    });

    equal(panel.currentStepLabel, 'Done/blocker');
    equal(
      panel.steps.find((step) => step.label === 'Done/blocker')?.state,
      'blocked'
    );
    match(panel.blocker ?? '', /dirty or conflicting/);
    match(panel.tokenSafetyNote, /not safe to re-review or merge/);
  });

  it('returns no-workspace blocker copy without implying active token use', () => {
    const panel = buildImplicationAutopilotPanelStatus(null);

    equal(panel.currentStepLabel, 'Implementation');
    match(panel.blocker ?? '', /No local workspace/);
    deepEqual(
      {
        label: panel.steps[0].label,
        state: panel.steps[0].state,
      },
      {
        label: 'Implementation',
        state: 'blocked',
      }
    );
    match(panel.tokenSafetyNote, /No Codex session is running/);
  });
});
