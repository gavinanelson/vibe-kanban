export type AutoReviewTransitionIssue = {
  id: string;
  status_id?: string | null;
};

export type GetAutoReviewTransitionIssueIdsInput = {
  issues: AutoReviewTransitionIssue[];
  previousStatusByIssueId: ReadonlyMap<string, string>;
  reviewStatusIds: ReadonlySet<string>;
  pendingIssueIds?: ReadonlySet<string>;
};

export const getAutoReviewTransitionIssueIds = ({
  issues,
  previousStatusByIssueId,
  reviewStatusIds,
  pendingIssueIds,
}: GetAutoReviewTransitionIssueIdsInput): string[] => {
  const issueIds = new Set<string>();

  for (const issue of issues) {
    const isInReview =
      !!issue.status_id && reviewStatusIds.has(issue.status_id);

    if (!isInReview) {
      continue;
    }

    if (pendingIssueIds?.has(issue.id)) {
      issueIds.add(issue.id);
      continue;
    }

    const previousStatusId = previousStatusByIssueId.get(issue.id);
    if (previousStatusId && previousStatusId !== issue.status_id) {
      issueIds.add(issue.id);
    }
  }

  return [...issueIds];
};
