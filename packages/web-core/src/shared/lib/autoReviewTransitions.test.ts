import { describe, it } from 'node:test';
import { deepEqual } from 'node:assert/strict';

import { getAutoReviewTransitionIssueIds } from './autoReviewTransitions.ts';

describe('getAutoReviewTransitionIssueIds', () => {
  it('keeps review transitions observed while loading available after loading finishes', () => {
    const reviewStatusIds = new Set(['review']);
    const pendingIssueIds = new Set<string>();

    const loadingTransitionIds = getAutoReviewTransitionIssueIds({
      issues: [{ id: 'issue-1', status_id: 'review' }],
      previousStatusByIssueId: new Map([['issue-1', 'todo']]),
      reviewStatusIds,
      pendingIssueIds,
    });

    for (const issueId of loadingTransitionIds) {
      pendingIssueIds.add(issueId);
    }

    deepEqual(loadingTransitionIds, ['issue-1']);
    deepEqual(
      getAutoReviewTransitionIssueIds({
        issues: [{ id: 'issue-1', status_id: 'review' }],
        previousStatusByIssueId: new Map([['issue-1', 'review']]),
        reviewStatusIds,
        pendingIssueIds,
      }),
      ['issue-1']
    );
  });

  it('does not keep a pending transition if the issue leaves review before loading finishes', () => {
    deepEqual(
      getAutoReviewTransitionIssueIds({
        issues: [{ id: 'issue-1', status_id: 'todo' }],
        previousStatusByIssueId: new Map([['issue-1', 'review']]),
        reviewStatusIds: new Set(['review']),
        pendingIssueIds: new Set(['issue-1']),
      }),
      []
    );
  });
});
