/// Opt-in GitHub → Vibe Kanban sync planning.
///
/// This module is deliberately side-effect-free: it takes a snapshot of GitHub issue
/// data and the existing Kanban state, then produces a [`SyncPlan`] describing what
/// mutations *would* need to happen. Callers decide whether/how to execute those ops.
///
/// Tracking issue: gavinanelson/implication#171
///
/// # Opt-in rule
/// Sync is only triggered when Gavin explicitly requests a Kanban sync run. Nothing
/// here runs automatically.
use std::collections::HashMap;

use uuid::Uuid;

// ---------------------------------------------------------------------------
// GitHub-side types (populated from GitHub REST/GraphQL responses)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubIssueState {
    Open,
    Closed,
}

#[derive(Debug, Clone)]
pub struct GitHubLabel {
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GitHubUser {
    pub login: String,
}

/// A lightweight reference to another GitHub issue (used for sub-issues / links).
#[derive(Debug, Clone)]
pub struct GitHubIssueRef {
    pub number: u64,
    pub url: String,
}

/// A comment on a GitHub issue that should be mirrored into Kanban.
/// Only comments marked for mirroring (e.g. status updates) are included —
/// callers filter before constructing the snapshot.
#[derive(Debug, Clone)]
pub struct GitHubMirrorComment {
    pub github_comment_id: u64,
    pub author_login: String,
    pub body: String,
}

/// Full snapshot of a GitHub issue as seen at sync time.
#[derive(Debug, Clone)]
pub struct GitHubIssue {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub body: Option<String>,
    pub state: GitHubIssueState,
    pub labels: Vec<GitHubLabel>,
    pub assignees: Vec<GitHubUser>,
    /// Direct children in GitHub's sub-issue hierarchy.
    pub sub_issues: Vec<GitHubIssueRef>,
    /// Issues that block this one (parsed from "blocked by #N" links or GitHub's
    /// tracked-in / sub-issue relationship metadata).
    pub blocked_by: Vec<GitHubIssueRef>,
    /// Peer issues that are related but not blocking.
    pub related_issues: Vec<GitHubIssueRef>,
    /// Status-update / notable comments to mirror as durable Kanban comments.
    pub mirror_comments: Vec<GitHubMirrorComment>,
}

// ---------------------------------------------------------------------------
// Kanban-side state (provided by caller from DB)
// ---------------------------------------------------------------------------

/// What we know about an existing Kanban issue that may already represent a
/// GitHub issue.  The `github_issue_number` is persisted in `extension_metadata`
/// (no schema migration required).
#[derive(Debug, Clone)]
pub struct KanbanIssueRef {
    pub id: Uuid,
    pub github_issue_number: Option<u64>,
    pub title: String,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Planned operations
// ---------------------------------------------------------------------------

/// A single mutation that the sync executor should apply to the Kanban board.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncOp {
    /// Create a brand-new Kanban issue for a GitHub issue that has no match yet.
    CreateIssue {
        github_number: u64,
        github_url: String,
        title: String,
        description: Option<String>,
        /// If this GitHub issue is itself a sub-issue, link it under the parent.
        parent_kanban_id: Option<Uuid>,
    },

    /// Bring an existing Kanban issue's title in line with GitHub.
    UpdateTitle {
        kanban_id: Uuid,
        new_title: String,
    },

    /// Bring an existing Kanban issue's description in line with GitHub.
    UpdateDescription {
        kanban_id: Uuid,
        new_description: Option<String>,
    },

    /// Establish a parent→child (sub-issue) link between two Kanban issues.
    LinkSubIssue {
        parent_kanban_id: Uuid,
        child_kanban_id: Uuid,
    },

    /// Mirror a GitHub "blocked-by" dependency as a Kanban blocking relationship.
    AddBlockingRelationship {
        /// The issue doing the blocking (i.e. must be done first).
        blocker_kanban_id: Uuid,
        /// The issue that is blocked.
        blocked_kanban_id: Uuid,
    },

    /// Mirror a GitHub "related" link as a Kanban related-issue link.
    AddRelatedLink {
        from_kanban_id: Uuid,
        to_kanban_id: Uuid,
    },

    /// Ensure a tag with `tag_name` exists and is attached to the Kanban issue.
    EnsureTag {
        kanban_id: Uuid,
        tag_name: String,
    },

    /// Append a durable status/comment from GitHub to the Kanban issue.
    /// The `github_comment_id` lets executors deduplicate on re-runs.
    /// TODO(#171): implement executor that writes Kanban comments via the remote API.
    AddComment {
        kanban_id: Uuid,
        github_comment_id: u64,
        author_login: String,
        body: String,
    },
}

/// The result of planning a sync run.
#[derive(Debug, Default)]
pub struct SyncPlan {
    /// Ordered list of mutations the executor should apply.
    pub ops: Vec<SyncOp>,
    /// GitHub issue numbers that were referenced (sub-issue / blocked-by / related)
    /// but were not present in the input snapshot.  Callers may fetch these and
    /// re-plan, or surface them as warnings.
    pub unresolved_github_refs: Vec<u64>,
    /// Human-readable notes about deferred work.  Printed in sync summaries.
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Planning logic
// ---------------------------------------------------------------------------

/// Build a [`SyncPlan`] from a GitHub snapshot and the current Kanban state.
///
/// The function is pure: it reads no DB, makes no HTTP calls, and has no side
/// effects.  The caller is responsible for executing the returned ops.
///
/// `github_issues` — every GitHub issue in scope for this sync run.
/// `existing_kanban` — every Kanban issue that might already track a GitHub issue.
pub fn plan_sync(
    github_issues: &[GitHubIssue],
    existing_kanban: &[KanbanIssueRef],
) -> SyncPlan {
    let mut plan = SyncPlan::default();

    // Build lookup: github issue number → kanban id (for already-tracked issues).
    let kanban_by_gh_number: HashMap<u64, &KanbanIssueRef> = existing_kanban
        .iter()
        .filter_map(|k| k.github_issue_number.map(|n| (n, k)))
        .collect();

    // We'll also need to know which GitHub issue numbers are in scope so we can
    // detect unresolved cross-references.
    let gh_numbers_in_scope: std::collections::HashSet<u64> =
        github_issues.iter().map(|i| i.number).collect();

    for issue in github_issues {
        let maybe_kanban = kanban_by_gh_number.get(&issue.number).copied();

        // --- Create or update the issue itself ---
        let kanban_id: Option<Uuid> = if let Some(kanban) = maybe_kanban {
            if kanban.title != issue.title {
                plan.ops.push(SyncOp::UpdateTitle {
                    kanban_id: kanban.id,
                    new_title: issue.title.clone(),
                });
            }
            if kanban.description.as_deref() != issue.body.as_deref() {
                plan.ops.push(SyncOp::UpdateDescription {
                    kanban_id: kanban.id,
                    new_description: issue.body.clone(),
                });
            }
            Some(kanban.id)
        } else {
            plan.ops.push(SyncOp::CreateIssue {
                github_number: issue.number,
                github_url: issue.url.clone(),
                title: issue.title.clone(),
                description: issue.body.clone(),
                parent_kanban_id: None, // sub-issue linkage handled below
            });
            None // kanban id not yet known; executor must resolve after create
        };

        // --- Sub-issues ---
        for sub_ref in &issue.sub_issues {
            if !gh_numbers_in_scope.contains(&sub_ref.number) {
                plan.unresolved_github_refs.push(sub_ref.number);
                continue;
            }
            if let (Some(parent_id), Some(child_kanban)) = (
                kanban_id,
                kanban_by_gh_number.get(&sub_ref.number).copied(),
            ) {
                plan.ops.push(SyncOp::LinkSubIssue {
                    parent_kanban_id: parent_id,
                    child_kanban_id: child_kanban.id,
                });
            }
            // If parent or child doesn't have a kanban_id yet the executor must
            // re-link after creating the missing issues.
        }

        // --- Blocked-by dependencies ---
        for blocker_ref in &issue.blocked_by {
            if !gh_numbers_in_scope.contains(&blocker_ref.number) {
                plan.unresolved_github_refs.push(blocker_ref.number);
                continue;
            }
            if let (Some(blocked_id), Some(blocker_kanban)) = (
                kanban_id,
                kanban_by_gh_number.get(&blocker_ref.number).copied(),
            ) {
                plan.ops.push(SyncOp::AddBlockingRelationship {
                    blocker_kanban_id: blocker_kanban.id,
                    blocked_kanban_id: blocked_id,
                });
            }
        }

        // --- Related links ---
        for related_ref in &issue.related_issues {
            if !gh_numbers_in_scope.contains(&related_ref.number) {
                plan.unresolved_github_refs.push(related_ref.number);
                continue;
            }
            if let (Some(from_id), Some(to_kanban)) = (
                kanban_id,
                kanban_by_gh_number.get(&related_ref.number).copied(),
            ) {
                plan.ops.push(SyncOp::AddRelatedLink {
                    from_kanban_id: from_id,
                    to_kanban_id: to_kanban.id,
                });
            }
        }

        // --- Labels → tags ---
        if let Some(kid) = kanban_id {
            for label in &issue.labels {
                plan.ops.push(SyncOp::EnsureTag {
                    kanban_id: kid,
                    tag_name: label.name.clone(),
                });
            }
        }

        // --- Mirror comments ---
        if let Some(kid) = kanban_id {
            for comment in &issue.mirror_comments {
                plan.ops.push(SyncOp::AddComment {
                    kanban_id: kid,
                    github_comment_id: comment.github_comment_id,
                    author_login: comment.author_login.clone(),
                    body: comment.body.clone(),
                });
            }
        }
    }

    // Deduplicate unresolved refs.
    plan.unresolved_github_refs.sort_unstable();
    plan.unresolved_github_refs.dedup();

    // Standing TODOs wired to the tracking issue.
    plan.notes.push(
        "TODO(#171): executor for SyncOp::CreateIssue via remote issues API".into(),
    );
    plan.notes.push(
        "TODO(#171): executor for SyncOp::AddComment — write durable Kanban comments".into(),
    );
    plan.notes.push(
        "TODO(#171): push-back from Kanban → GitHub (status, labels, assignees)".into(),
    );
    plan.notes.push(
        "TODO(#171): GitHub GraphQL sub-issue query to populate GitHubIssue.sub_issues".into(),
    );

    plan
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_uuid(n: u64) -> Uuid {
        // Deterministic UUIDs for test readability.
        Uuid::from_u128(n as u128)
    }

    fn simple_gh_issue(number: u64, title: &str) -> GitHubIssue {
        GitHubIssue {
            number,
            url: format!("https://github.com/owner/repo/issues/{number}"),
            title: title.to_string(),
            body: None,
            state: GitHubIssueState::Open,
            labels: vec![],
            assignees: vec![],
            sub_issues: vec![],
            blocked_by: vec![],
            related_issues: vec![],
            mirror_comments: vec![],
        }
    }

    fn kanban_ref(id_n: u64, gh_number: u64, title: &str) -> KanbanIssueRef {
        KanbanIssueRef {
            id: make_uuid(id_n),
            github_issue_number: Some(gh_number),
            title: title.to_string(),
            description: None,
        }
    }

    // --- CreateIssue ---

    #[test]
    fn creates_issue_when_no_kanban_match() {
        let gh = [simple_gh_issue(42, "New feature")];
        let plan = plan_sync(&gh, &[]);

        assert_eq!(plan.ops.len(), 1, "expected exactly one op");
        assert!(
            matches!(
                &plan.ops[0],
                SyncOp::CreateIssue { github_number: 42, .. }
            ),
            "expected CreateIssue"
        );
        // Notes for TODO items should always be present.
        assert!(!plan.notes.is_empty());
    }

    #[test]
    fn no_create_when_already_tracked() {
        let gh = [simple_gh_issue(7, "Same title")];
        let kanban = [kanban_ref(1, 7, "Same title")];
        let plan = plan_sync(&gh, &kanban);

        let creates: Vec<_> = plan
            .ops
            .iter()
            .filter(|op| matches!(op, SyncOp::CreateIssue { .. }))
            .collect();
        assert!(creates.is_empty(), "should not create an already-tracked issue");
    }

    // --- UpdateTitle ---

    #[test]
    fn emits_title_update_when_title_differs() {
        let gh = [simple_gh_issue(10, "Renamed title")];
        let kanban = [kanban_ref(1, 10, "Old title")];
        let plan = plan_sync(&gh, &kanban);

        let updates: Vec<_> = plan
            .ops
            .iter()
            .filter(|op| matches!(op, SyncOp::UpdateTitle { .. }))
            .collect();
        assert_eq!(updates.len(), 1);
        assert!(
            matches!(
                updates[0],
                SyncOp::UpdateTitle { new_title, .. } if new_title == "Renamed title"
            )
        );
    }

    #[test]
    fn no_title_update_when_title_matches() {
        let gh = [simple_gh_issue(10, "Same")];
        let kanban = [kanban_ref(1, 10, "Same")];
        let plan = plan_sync(&gh, &kanban);

        assert!(
            !plan
                .ops
                .iter()
                .any(|op| matches!(op, SyncOp::UpdateTitle { .. })),
            "should not emit title update when title is unchanged"
        );
    }

    // --- UpdateDescription ---

    #[test]
    fn emits_description_update_when_body_differs() {
        let mut gh = simple_gh_issue(5, "Title");
        gh.body = Some("New body".into());
        let kanban = [KanbanIssueRef {
            id: make_uuid(1),
            github_issue_number: Some(5),
            title: "Title".into(),
            description: Some("Old body".into()),
        }];
        let plan = plan_sync(&[gh], &kanban);

        assert!(
            plan.ops
                .iter()
                .any(|op| matches!(op, SyncOp::UpdateDescription { new_description: Some(d), .. } if d == "New body")),
            "expected UpdateDescription"
        );
    }

    // --- LinkSubIssue ---

    #[test]
    fn emits_sub_issue_link_when_both_tracked() {
        let mut parent = simple_gh_issue(1, "Parent");
        parent.sub_issues = vec![GitHubIssueRef {
            number: 2,
            url: "https://github.com/owner/repo/issues/2".into(),
        }];
        let child = simple_gh_issue(2, "Child");

        let kanban = [kanban_ref(10, 1, "Parent"), kanban_ref(20, 2, "Child")];
        let plan = plan_sync(&[parent, child], &kanban);

        assert!(
            plan.ops.iter().any(|op| matches!(
                op,
                SyncOp::LinkSubIssue {
                    parent_kanban_id,
                    child_kanban_id,
                } if *parent_kanban_id == make_uuid(10) && *child_kanban_id == make_uuid(20)
            )),
            "expected LinkSubIssue"
        );
    }

    #[test]
    fn unresolved_ref_when_sub_issue_not_in_scope() {
        let mut parent = simple_gh_issue(1, "Parent");
        parent.sub_issues = vec![GitHubIssueRef {
            number: 99,
            url: "https://github.com/owner/repo/issues/99".into(),
        }];
        let plan = plan_sync(&[parent], &[]);

        assert!(
            plan.unresolved_github_refs.contains(&99),
            "should record unresolved sub-issue ref"
        );
    }

    // --- Blocking relationship ---

    #[test]
    fn emits_blocking_relationship_when_both_tracked() {
        let mut issue_a = simple_gh_issue(3, "A");
        issue_a.blocked_by = vec![GitHubIssueRef {
            number: 4,
            url: "https://github.com/owner/repo/issues/4".into(),
        }];
        let issue_b = simple_gh_issue(4, "B");

        let kanban = [kanban_ref(30, 3, "A"), kanban_ref(40, 4, "B")];
        let plan = plan_sync(&[issue_a, issue_b], &kanban);

        assert!(
            plan.ops.iter().any(|op| matches!(
                op,
                SyncOp::AddBlockingRelationship {
                    blocker_kanban_id,
                    blocked_kanban_id,
                } if *blocker_kanban_id == make_uuid(40) && *blocked_kanban_id == make_uuid(30)
            )),
            "expected AddBlockingRelationship with correct blocker/blocked direction"
        );
    }

    // --- Related link ---

    #[test]
    fn emits_related_link_when_both_tracked() {
        let mut issue_x = simple_gh_issue(5, "X");
        issue_x.related_issues = vec![GitHubIssueRef {
            number: 6,
            url: "https://github.com/owner/repo/issues/6".into(),
        }];
        let issue_y = simple_gh_issue(6, "Y");

        let kanban = [kanban_ref(50, 5, "X"), kanban_ref(60, 6, "Y")];
        let plan = plan_sync(&[issue_x, issue_y], &kanban);

        assert!(
            plan.ops
                .iter()
                .any(|op| matches!(op, SyncOp::AddRelatedLink { .. })),
            "expected AddRelatedLink"
        );
    }

    // --- Labels → tags ---

    #[test]
    fn emits_ensure_tag_for_each_label() {
        let mut gh = simple_gh_issue(7, "Tagged");
        gh.labels = vec![
            GitHubLabel { name: "bug".into(), color: Some("d73a4a".into()) },
            GitHubLabel { name: "p1".into(), color: None },
        ];
        let kanban = [kanban_ref(70, 7, "Tagged")];
        let plan = plan_sync(&[gh], &kanban);

        let tags: Vec<_> = plan
            .ops
            .iter()
            .filter_map(|op| {
                if let SyncOp::EnsureTag { tag_name, .. } = op {
                    Some(tag_name.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(tags.contains(&"bug"), "expected 'bug' tag");
        assert!(tags.contains(&"p1"), "expected 'p1' tag");
    }

    // --- Mirror comments ---

    #[test]
    fn emits_add_comment_for_mirror_comments() {
        let mut gh = simple_gh_issue(8, "With comment");
        gh.mirror_comments = vec![GitHubMirrorComment {
            github_comment_id: 9999,
            author_login: "alice".into(),
            body: "Status: shipped".into(),
        }];
        let kanban = [kanban_ref(80, 8, "With comment")];
        let plan = plan_sync(&[gh], &kanban);

        assert!(
            plan.ops.iter().any(|op| matches!(
                op,
                SyncOp::AddComment { github_comment_id: 9999, .. }
            )),
            "expected AddComment for mirror comment"
        );
    }

    // --- Deduplication of unresolved refs ---

    #[test]
    fn deduplicates_unresolved_refs() {
        let mut issue_a = simple_gh_issue(1, "A");
        issue_a.blocked_by = vec![GitHubIssueRef {
            number: 99,
            url: "https://github.com/owner/repo/issues/99".into(),
        }];
        let mut issue_b = simple_gh_issue(2, "B");
        issue_b.related_issues = vec![GitHubIssueRef {
            number: 99,
            url: "https://github.com/owner/repo/issues/99".into(),
        }];
        let plan = plan_sync(&[issue_a, issue_b], &[]);

        let count_99 = plan
            .unresolved_github_refs
            .iter()
            .filter(|&&n| n == 99)
            .count();
        assert_eq!(count_99, 1, "unresolved refs should be deduplicated");
    }

    // --- Empty inputs ---

    #[test]
    fn empty_inputs_produce_empty_plan() {
        let plan = plan_sync(&[], &[]);
        assert!(plan.ops.is_empty());
        assert!(plan.unresolved_github_refs.is_empty());
        assert!(!plan.notes.is_empty(), "notes (TODOs) should always be populated");
    }
}
