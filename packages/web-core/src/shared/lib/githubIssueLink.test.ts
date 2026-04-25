import { describe, expect, it } from 'vitest';

import {
  clearGitHubIssueLink,
  createGitHubIssueLinkFromSummary,
  getGitHubIssueLink,
  parseGitHubIssueUrl,
  setGitHubIssueLink,
} from './githubIssueLink';

describe('githubIssueLink', () => {
  it('parses a GitHub issue URL into normalized link data', () => {
    expect(
      parseGitHubIssueUrl(
        'https://github.com/openai/openai/issues/123?foo=bar#section'
      )
    ).toEqual({
      repo_full_name: 'openai/openai',
      issue_number: 123,
      issue_url: 'https://github.com/openai/openai/issues/123',
    });
  });

  it('rejects non-issue URLs', () => {
    expect(
      parseGitHubIssueUrl('https://github.com/openai/openai/pull/123')
    ).toBeNull();
    expect(parseGitHubIssueUrl('not a url')).toBeNull();
  });

  it('sets and reads github_link while preserving unrelated metadata', () => {
    const extensionMetadata = {
      foo: 'bar',
      nested: { ok: true },
    };

    const nextMetadata = setGitHubIssueLink(extensionMetadata, {
      repo_full_name: '/openai/openai/',
      issue_number: 123,
      issue_url: 'https://github.com/openai/openai/issues/123/',
      last_seen_title: 'Fix auth bug',
    });

    expect(nextMetadata).toEqual({
      foo: 'bar',
      nested: { ok: true },
      github_link: {
        repo_full_name: 'openai/openai',
        issue_number: 123,
        issue_url: 'https://github.com/openai/openai/issues/123',
        last_seen_title: 'Fix auth bug',
      },
    });

    expect(getGitHubIssueLink(nextMetadata)).toEqual({
      repo_full_name: 'openai/openai',
      issue_number: 123,
      issue_url: 'https://github.com/openai/openai/issues/123',
      last_seen_title: 'Fix auth bug',
    });
  });

  it('clears github_link and returns null when metadata becomes empty', () => {
    expect(
      clearGitHubIssueLink({
        github_link: {
          repo_full_name: 'openai/openai',
          issue_number: 123,
          issue_url: 'https://github.com/openai/openai/issues/123',
        },
      })
    ).toBeNull();
  });

  it('clears github_link without removing unrelated metadata', () => {
    expect(
      clearGitHubIssueLink({
        github_link: {
          repo_full_name: 'openai/openai',
          issue_number: 123,
          issue_url: 'https://github.com/openai/openai/issues/123',
        },
        foo: 'bar',
      })
    ).toEqual({
      foo: 'bar',
    });
  });

  it('creates a persisted link payload from an issue summary', () => {
    expect(
      createGitHubIssueLinkFromSummary({
        repo_full_name: 'openai/openai',
        issue_number: 123,
        issue_url: 'https://github.com/openai/openai/issues/123',
        title: 'Fix auth bug',
        state: 'OPEN',
        updated_at: '2026-04-22T00:00:00Z',
      })
    ).toEqual({
      repo_full_name: 'openai/openai',
      issue_number: 123,
      issue_url: 'https://github.com/openai/openai/issues/123',
      last_seen_title: 'Fix auth bug',
      last_seen_state: 'OPEN',
      last_seen_updated_at: '2026-04-22T00:00:00Z',
    });
  });
});
