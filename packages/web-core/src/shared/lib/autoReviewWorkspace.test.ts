import { describe, it } from 'node:test';
import { equal } from 'node:assert/strict';

import { getAutoReviewLocalWorkspaceId } from './autoReviewWorkspace';

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
