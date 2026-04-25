import { describe, it } from 'node:test';
import { deepEqual, equal } from 'node:assert/strict';

import {
  getAutoReviewLocalWorkspaceId,
  getAutoReviewWorkspaceResolution,
} from './autoReviewWorkspace.ts';

describe('getAutoReviewLocalWorkspaceId', () => {
  it('selects the first non-archived linked workspace in the local context', () => {
    equal(
      getAutoReviewLocalWorkspaceId(
        [
          {
            archived: false,
            local_workspace_id: 'remote-workspace',
          },
          {
            archived: false,
            local_workspace_id: 'local-workspace',
          },
        ],
        new Map([['local-workspace', {}]])
      ),
      'local-workspace'
    );
  });

  it('does not select archived or remote-only linked workspaces', () => {
    equal(
      getAutoReviewLocalWorkspaceId(
        [
          {
            archived: false,
            local_workspace_id: 'remote-workspace',
          },
          {
            archived: true,
            local_workspace_id: 'local-workspace',
          },
        ],
        new Map([['local-workspace', {}]])
      ),
      null
    );
  });
});

describe('getAutoReviewWorkspaceResolution', () => {
  it('returns pending-local-workspace when a linked local workspace is not loaded yet', () => {
    deepEqual(
      getAutoReviewWorkspaceResolution(
        [
          {
            archived: false,
            local_workspace_id: 'local-workspace',
          },
        ],
        new Map()
      ),
      {
        state: 'pending-local-workspace',
      }
    );
  });

  it('returns no-linked-workspace when no non-archived workspace has a local id', () => {
    deepEqual(
      getAutoReviewWorkspaceResolution(
        [
          {
            archived: false,
            local_workspace_id: null,
          },
          {
            archived: true,
            local_workspace_id: 'archived-local-workspace',
          },
        ],
        new Map([['archived-local-workspace', {}]])
      ),
      {
        state: 'no-linked-workspace',
      }
    );
  });
});
