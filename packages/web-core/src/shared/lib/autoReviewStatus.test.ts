import { describe, it } from 'node:test';
import { deepEqual, equal } from 'node:assert/strict';

import { deriveAutoReviewStatus } from './autoReviewStatus.ts';

describe('deriveAutoReviewStatus', () => {
  it('returns starting while the app is creating the review session', () => {
    deepEqual(
      deriveAutoReviewStatus({
        issueStatusName: 'In review',
        workspace: { isRunning: false },
        pendingReview: { state: 'starting' },
      }),
      {
        state: 'starting',
        label: 'Review starting',
      }
    );
  });

  it('returns running for a pending review once the workspace is running', () => {
    deepEqual(
      deriveAutoReviewStatus({
        issueStatusName: 'Review',
        workspace: { isRunning: true },
        pendingReview: { state: 'running' },
      }),
      {
        state: 'running',
        label: 'Review running',
      }
    );
  });

  it('returns completed when the requested review process completes', () => {
    deepEqual(
      deriveAutoReviewStatus({
        issueStatusName: 'Review',
        workspace: {
          isRunning: false,
          latestProcessStatus: 'completed',
          latestProcessCompletedAt: '2026-04-24T20:15:00.000Z',
        },
        pendingReview: {
          state: 'running',
          requestedAt: '2026-04-24T20:00:00.000Z',
        },
      }),
      {
        state: 'completed',
        label: 'Review complete',
      }
    );
  });

  it('does not show stale completion from before the review was requested', () => {
    equal(
      deriveAutoReviewStatus({
        issueStatusName: 'Review',
        workspace: {
          isRunning: false,
          latestProcessStatus: 'completed',
          latestProcessCompletedAt: '2026-04-24T19:59:00.000Z',
        },
        pendingReview: {
          state: 'running',
          requestedAt: '2026-04-24T20:00:00.000Z',
        },
      })?.state,
      'running'
    );
  });

  it('returns null outside review columns unless a review is pending', () => {
    equal(
      deriveAutoReviewStatus({
        issueStatusName: 'Done',
        workspace: { isRunning: false, latestProcessStatus: 'completed' },
        pendingReview: null,
      }),
      null
    );
  });
});
