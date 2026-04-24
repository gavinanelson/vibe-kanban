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
use std::collections::{HashMap, HashSet};

use api_types::{
    CreateIssueCommentRequest, CreateIssueRelationshipRequest, CreateIssueRequest,
    IssueRelationshipType as ApiIssueRelationshipType, UpdateIssueRequest,
};
use serde_json::json;
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
    /// GitHub relationship metadata this runtime could not fetch safely.
    pub unsupported_relations: Vec<UnsupportedGitHubRelation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHubRelationKind {
    SubIssues,
    BlockedBy,
    Related,
    Comments,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedGitHubRelation {
    pub kind: GitHubRelationKind,
    pub reason: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KanbanRelationshipType {
    Blocking,
    Related,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KanbanRelationshipRef {
    pub from_kanban_id: Uuid,
    pub to_kanban_id: Uuid,
    pub relationship_type: KanbanRelationshipType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KanbanMirroredCommentRef {
    pub kanban_id: Uuid,
    pub github_comment_id: u64,
}

#[derive(Debug, Clone, Default)]
pub struct KanbanSyncState {
    pub issues: Vec<KanbanIssueRef>,
    pub relationships: Vec<KanbanRelationshipRef>,
    pub mirrored_comments: Vec<KanbanMirroredCommentRef>,
}

// ---------------------------------------------------------------------------
// Planned operations
// ---------------------------------------------------------------------------

/// A single mutation that the sync executor should apply to the Kanban board.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncOp {
    /// Create a brand-new Kanban issue for a GitHub issue that has no match yet.
    CreateIssue {
        /// Client-generated ID reserved during planning so later ops can refer to it.
        kanban_id: Uuid,
        github_number: u64,
        github_url: String,
        title: String,
        description: Option<String>,
        /// If this GitHub issue is itself a sub-issue, link it under the parent.
        parent_kanban_id: Option<Uuid>,
    },

    /// Bring an existing Kanban issue's title in line with GitHub.
    UpdateTitle { kanban_id: Uuid, new_title: String },

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
    EnsureTag { kanban_id: Uuid, tag_name: String },

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
    /// Work that is intentionally not guessed by this sync slice.
    pub unsupported: Vec<UnsupportedSyncItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedSyncItem {
    GitHubRelation {
        github_number: u64,
        kind: GitHubRelationKind,
        reason: String,
    },
    Tags {
        kanban_id: Uuid,
        tag_name: String,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct PrepareSyncOptions {
    pub project_id: Uuid,
    pub default_status_id: Uuid,
    pub default_sort_order: f64,
}

#[derive(Debug, Clone)]
pub enum PreparedSyncAction {
    CreateIssue(CreateIssueRequest),
    UpdateIssue {
        issue_id: Uuid,
        request: UpdateIssueRequest,
    },
    CreateIssueRelationship(CreateIssueRelationshipRequest),
    CreateIssueComment(CreateIssueCommentRequest),
    Unsupported(UnsupportedSyncItem),
}

#[derive(Debug, Clone, Default)]
pub struct PreparedSync {
    pub actions: Vec<PreparedSyncAction>,
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
pub fn plan_sync(github_issues: &[GitHubIssue], existing_kanban: &[KanbanIssueRef]) -> SyncPlan {
    plan_sync_with_state(
        github_issues,
        &KanbanSyncState {
            issues: existing_kanban.to_vec(),
            relationships: Vec::new(),
            mirrored_comments: Vec::new(),
        },
    )
}

pub fn plan_sync_with_state(github_issues: &[GitHubIssue], state: &KanbanSyncState) -> SyncPlan {
    let mut plan = SyncPlan::default();

    // Build lookup: github issue number → kanban id (for already-tracked issues).
    let kanban_by_gh_number: HashMap<u64, &KanbanIssueRef> = state
        .issues
        .iter()
        .filter_map(|k| k.github_issue_number.map(|n| (n, k)))
        .collect();
    let mut planned_kanban_by_gh_number: HashMap<u64, Uuid> = HashMap::new();
    for issue in github_issues {
        let kanban_id = kanban_by_gh_number
            .get(&issue.number)
            .map(|kanban| kanban.id)
            .unwrap_or_else(Uuid::new_v4);
        planned_kanban_by_gh_number.insert(issue.number, kanban_id);
    }

    // We'll also need to know which GitHub issue numbers are in scope so we can
    // detect unresolved cross-references.
    let gh_numbers_in_scope: HashSet<u64> = github_issues.iter().map(|i| i.number).collect();

    let existing_relationships: HashSet<KanbanRelationshipRef> =
        state.relationships.iter().copied().collect();
    let existing_comments: HashSet<KanbanMirroredCommentRef> =
        state.mirrored_comments.iter().copied().collect();

    for issue in github_issues {
        let maybe_kanban = kanban_by_gh_number.get(&issue.number).copied();
        let planned_kanban_id = planned_kanban_by_gh_number[&issue.number];

        for unsupported in &issue.unsupported_relations {
            plan.unsupported.push(UnsupportedSyncItem::GitHubRelation {
                github_number: issue.number,
                kind: unsupported.kind,
                reason: unsupported.reason.clone(),
            });
            plan.notes.push(format!(
                "GitHub issue #{} {:?} needs manual sync: {}",
                issue.number, unsupported.kind, unsupported.reason
            ));
        }

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
            let parent_kanban_id = github_issues
                .iter()
                .find(|candidate| {
                    candidate
                        .sub_issues
                        .iter()
                        .any(|sub_ref| sub_ref.number == issue.number)
                })
                .and_then(|parent| planned_kanban_by_gh_number.get(&parent.number).copied());
            plan.ops.push(SyncOp::CreateIssue {
                kanban_id: planned_kanban_id,
                github_number: issue.number,
                github_url: issue.url.clone(),
                title: issue.title.clone(),
                description: issue.body.clone(),
                parent_kanban_id,
            });
            Some(planned_kanban_id)
        };

        // --- Sub-issues ---
        for sub_ref in &issue.sub_issues {
            if !gh_numbers_in_scope.contains(&sub_ref.number) {
                plan.unresolved_github_refs.push(sub_ref.number);
                continue;
            }
            if let (Some(parent_id), Some(child_kanban)) =
                (kanban_id, kanban_by_gh_number.get(&sub_ref.number).copied())
                && child_kanban.id != parent_id
            {
                plan.ops.push(SyncOp::LinkSubIssue {
                    parent_kanban_id: parent_id,
                    child_kanban_id: child_kanban.id,
                });
            }
        }

        // --- Blocked-by dependencies ---
        for blocker_ref in &issue.blocked_by {
            if !gh_numbers_in_scope.contains(&blocker_ref.number) {
                plan.unresolved_github_refs.push(blocker_ref.number);
                continue;
            }
            if let (Some(blocked_id), Some(blocker_kanban)) = (
                kanban_id,
                planned_kanban_by_gh_number
                    .get(&blocker_ref.number)
                    .copied(),
            ) {
                let relationship = KanbanRelationshipRef {
                    from_kanban_id: blocker_kanban,
                    to_kanban_id: blocked_id,
                    relationship_type: KanbanRelationshipType::Blocking,
                };
                if !existing_relationships.contains(&relationship) {
                    plan.ops.push(SyncOp::AddBlockingRelationship {
                        blocker_kanban_id: relationship.from_kanban_id,
                        blocked_kanban_id: relationship.to_kanban_id,
                    });
                }
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
                planned_kanban_by_gh_number
                    .get(&related_ref.number)
                    .copied(),
            ) {
                let relationship = KanbanRelationshipRef {
                    from_kanban_id: from_id,
                    to_kanban_id: to_kanban,
                    relationship_type: KanbanRelationshipType::Related,
                };
                if !existing_relationships.contains(&relationship) {
                    plan.ops.push(SyncOp::AddRelatedLink {
                        from_kanban_id: relationship.from_kanban_id,
                        to_kanban_id: relationship.to_kanban_id,
                    });
                }
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
                let mirrored = KanbanMirroredCommentRef {
                    kanban_id: kid,
                    github_comment_id: comment.github_comment_id,
                };
                if !existing_comments.contains(&mirrored) {
                    plan.ops.push(SyncOp::AddComment {
                        kanban_id: kid,
                        github_comment_id: comment.github_comment_id,
                        author_login: comment.author_login.clone(),
                        body: comment.body.clone(),
                    });
                }
            }
        }
    }

    // Deduplicate unresolved refs.
    plan.unresolved_github_refs.sort_unstable();
    plan.unresolved_github_refs.dedup();

    plan.notes
        .push("TODO(#171): push-back from Kanban → GitHub (status, labels, assignees)".into());

    plan
}

pub fn prepare_sync_actions(plan: &SyncPlan, options: PrepareSyncOptions) -> PreparedSync {
    let mut issue_actions = Vec::new();
    let mut relationship_actions = Vec::new();
    let mut comment_actions = Vec::new();
    let mut unsupported_actions = Vec::new();

    for op in &plan.ops {
        match op {
            SyncOp::CreateIssue {
                kanban_id,
                github_number,
                github_url,
                title,
                description,
                parent_kanban_id,
            } => issue_actions.push(PreparedSyncAction::CreateIssue(CreateIssueRequest {
                id: Some(*kanban_id),
                project_id: options.project_id,
                status_id: options.default_status_id,
                title: title.clone(),
                description: description.clone(),
                priority: None,
                start_date: None,
                target_date: None,
                completed_at: None,
                sort_order: options.default_sort_order,
                parent_issue_id: *parent_kanban_id,
                parent_issue_sort_order: None,
                extension_metadata: github_extension_metadata(*github_number, github_url),
            })),
            SyncOp::UpdateTitle {
                kanban_id,
                new_title,
            } => issue_actions.push(PreparedSyncAction::UpdateIssue {
                issue_id: *kanban_id,
                request: UpdateIssueRequest {
                    title: Some(new_title.clone()),
                    ..empty_update_issue_request()
                },
            }),
            SyncOp::UpdateDescription {
                kanban_id,
                new_description,
            } => issue_actions.push(PreparedSyncAction::UpdateIssue {
                issue_id: *kanban_id,
                request: UpdateIssueRequest {
                    description: Some(new_description.clone()),
                    ..empty_update_issue_request()
                },
            }),
            SyncOp::LinkSubIssue {
                parent_kanban_id,
                child_kanban_id,
            } => issue_actions.push(PreparedSyncAction::UpdateIssue {
                issue_id: *child_kanban_id,
                request: UpdateIssueRequest {
                    parent_issue_id: Some(Some(*parent_kanban_id)),
                    ..empty_update_issue_request()
                },
            }),
            SyncOp::AddBlockingRelationship {
                blocker_kanban_id,
                blocked_kanban_id,
            } => relationship_actions.push(PreparedSyncAction::CreateIssueRelationship(
                CreateIssueRelationshipRequest {
                    id: None,
                    issue_id: *blocker_kanban_id,
                    related_issue_id: *blocked_kanban_id,
                    relationship_type: ApiIssueRelationshipType::Blocking,
                },
            )),
            SyncOp::AddRelatedLink {
                from_kanban_id,
                to_kanban_id,
            } => relationship_actions.push(PreparedSyncAction::CreateIssueRelationship(
                CreateIssueRelationshipRequest {
                    id: None,
                    issue_id: *from_kanban_id,
                    related_issue_id: *to_kanban_id,
                    relationship_type: ApiIssueRelationshipType::Related,
                },
            )),
            SyncOp::EnsureTag {
                kanban_id,
                tag_name,
            } => unsupported_actions.push(PreparedSyncAction::Unsupported(
                UnsupportedSyncItem::Tags {
                    kanban_id: *kanban_id,
                    tag_name: tag_name.clone(),
                    reason: "Kanban tag API mapping is not available in github_sync_plan".into(),
                },
            )),
            SyncOp::AddComment {
                kanban_id,
                github_comment_id,
                author_login,
                body,
            } => comment_actions.push(PreparedSyncAction::CreateIssueComment(
                CreateIssueCommentRequest {
                    id: None,
                    issue_id: *kanban_id,
                    message: format_mirrored_github_comment(*github_comment_id, author_login, body),
                    parent_id: None,
                },
            )),
        }
    }

    unsupported_actions.extend(
        plan.unsupported
            .iter()
            .cloned()
            .map(PreparedSyncAction::Unsupported),
    );

    let mut actions = issue_actions;
    actions.extend(relationship_actions);
    actions.extend(comment_actions);
    actions.extend(unsupported_actions);

    PreparedSync { actions }
}

pub fn format_mirrored_github_comment(
    github_comment_id: u64,
    author_login: &str,
    body: &str,
) -> String {
    format!(
        "GitHub comment by @{author_login}:\n\n{body}\n\n<!-- vibe-kanban-github-comment-id:{github_comment_id} -->"
    )
}

pub fn github_comment_id_from_kanban_message(message: &str) -> Option<u64> {
    let marker = "vibe-kanban-github-comment-id:";
    let start = message.find(marker)? + marker.len();
    let tail = &message[start..];
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn github_extension_metadata(github_number: u64, github_url: &str) -> serde_json::Value {
    json!({
        "github": {
            "issue_number": github_number,
            "url": github_url,
            "sync": {
                "source": "github",
                "mode": "opt_in"
            }
        }
    })
}

fn empty_update_issue_request() -> UpdateIssueRequest {
    UpdateIssueRequest {
        status_id: None,
        title: None,
        description: None,
        priority: None,
        start_date: None,
        target_date: None,
        completed_at: None,
        sort_order: None,
        parent_issue_id: None,
        parent_issue_sort_order: None,
        extension_metadata: None,
    }
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
            unsupported_relations: vec![],
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
                SyncOp::CreateIssue {
                    github_number: 42,
                    ..
                }
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
        assert!(
            creates.is_empty(),
            "should not create an already-tracked issue"
        );
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
        assert!(matches!(
            updates[0],
            SyncOp::UpdateTitle { new_title, .. } if new_title == "Renamed title"
        ));
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
            GitHubLabel {
                name: "bug".into(),
                color: Some("d73a4a".into()),
            },
            GitHubLabel {
                name: "p1".into(),
                color: None,
            },
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
                SyncOp::AddComment {
                    github_comment_id: 9999,
                    ..
                }
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
        assert!(
            !plan.notes.is_empty(),
            "notes (TODOs) should always be populated"
        );
    }

    #[test]
    fn creates_sub_issue_with_reserved_parent_id_when_both_missing() {
        let mut parent = simple_gh_issue(1, "Parent");
        parent.sub_issues = vec![GitHubIssueRef {
            number: 2,
            url: "https://github.com/owner/repo/issues/2".into(),
        }];
        let child = simple_gh_issue(2, "Child");

        let plan = plan_sync(&[parent, child], &[]);

        let parent_id = plan
            .ops
            .iter()
            .find_map(|op| match op {
                SyncOp::CreateIssue {
                    github_number: 1,
                    kanban_id,
                    ..
                } => Some(*kanban_id),
                _ => None,
            })
            .expect("parent create should reserve a kanban id");

        assert!(
            plan.ops.iter().any(|op| matches!(
                op,
                SyncOp::CreateIssue {
                    github_number: 2,
                    parent_kanban_id: Some(id),
                    ..
                } if *id == parent_id
            )),
            "child create should point at the reserved parent id"
        );
    }

    #[test]
    fn skips_relationships_and_comments_already_mirrored_in_state() {
        let mut issue = simple_gh_issue(10, "Blocked");
        issue.blocked_by = vec![GitHubIssueRef {
            number: 11,
            url: "https://github.com/owner/repo/issues/11".into(),
        }];
        issue.related_issues = vec![GitHubIssueRef {
            number: 12,
            url: "https://github.com/owner/repo/issues/12".into(),
        }];
        issue.mirror_comments = vec![GitHubMirrorComment {
            github_comment_id: 500,
            author_login: "alice".into(),
            body: "Already imported".into(),
        }];

        let blocker = simple_gh_issue(11, "Blocker");
        let related = simple_gh_issue(12, "Related");
        let state = KanbanSyncState {
            issues: vec![
                kanban_ref(10, 10, "Blocked"),
                kanban_ref(11, 11, "Blocker"),
                kanban_ref(12, 12, "Related"),
            ],
            relationships: vec![
                KanbanRelationshipRef {
                    from_kanban_id: make_uuid(11),
                    to_kanban_id: make_uuid(10),
                    relationship_type: KanbanRelationshipType::Blocking,
                },
                KanbanRelationshipRef {
                    from_kanban_id: make_uuid(10),
                    to_kanban_id: make_uuid(12),
                    relationship_type: KanbanRelationshipType::Related,
                },
            ],
            mirrored_comments: vec![KanbanMirroredCommentRef {
                kanban_id: make_uuid(10),
                github_comment_id: 500,
            }],
        };

        let plan = plan_sync_with_state(&[issue, blocker, related], &state);

        assert!(
            !plan
                .ops
                .iter()
                .any(|op| matches!(op, SyncOp::AddBlockingRelationship { .. })),
            "existing blocking relationship should not be duplicated"
        );
        assert!(
            !plan
                .ops
                .iter()
                .any(|op| matches!(op, SyncOp::AddRelatedLink { .. })),
            "existing related relationship should not be duplicated"
        );
        assert!(
            !plan
                .ops
                .iter()
                .any(|op| matches!(op, SyncOp::AddComment { .. })),
            "existing mirrored comment should not be duplicated"
        );
    }

    #[test]
    fn prepares_remote_api_actions_for_creates_updates_relationships_and_comments() {
        let project_id = make_uuid(1000);
        let status_id = make_uuid(1001);

        let mut issue = simple_gh_issue(21, "Parent");
        issue.body = Some("Canonical body".into());
        issue.sub_issues = vec![GitHubIssueRef {
            number: 22,
            url: "https://github.com/owner/repo/issues/22".into(),
        }];
        issue.mirror_comments = vec![GitHubMirrorComment {
            github_comment_id: 2222,
            author_login: "octocat".into(),
            body: "Kanban update: ready".into(),
        }];
        let child = simple_gh_issue(22, "Child");

        let plan = plan_sync(&[issue, child], &[]);
        let prepared = prepare_sync_actions(
            &plan,
            PrepareSyncOptions {
                project_id,
                default_status_id: status_id,
                default_sort_order: 0.0,
            },
        );

        assert_eq!(prepared.actions.len(), 3);
        assert!(prepared.actions.iter().any(|action| matches!(
            action,
            PreparedSyncAction::CreateIssue(request)
                if request.project_id == project_id
                    && request.status_id == status_id
                    && request.extension_metadata["github"]["issue_number"] == 21
        )));
        assert!(prepared.actions.iter().any(|action| matches!(
            action,
            PreparedSyncAction::CreateIssue(request)
                if request.parent_issue_id.is_some()
                    && request.extension_metadata["github"]["issue_number"] == 22
        )));
        assert!(prepared.actions.iter().any(|action| matches!(
            action,
            PreparedSyncAction::CreateIssueComment(request)
                if request.message.contains("vibe-kanban-github-comment-id:2222")
                    && request.message.contains("@octocat")
        )));
    }

    #[test]
    fn records_unsupported_relation_fetches_as_manual_sync_notes() {
        let mut issue = simple_gh_issue(31, "Needs GraphQL");
        issue.unsupported_relations = vec![UnsupportedGitHubRelation {
            kind: GitHubRelationKind::SubIssues,
            reason: "GitHub GraphQL subIssues query unavailable in this runtime".into(),
        }];

        let plan = plan_sync(&[issue], &[]);

        assert!(plan.unsupported.iter().any(|unsupported| matches!(
            unsupported,
            UnsupportedSyncItem::GitHubRelation {
                github_number: 31,
                kind: GitHubRelationKind::SubIssues,
                ..
            }
        )));
        assert!(
            plan.notes
                .iter()
                .any(|note| note.contains("needs manual sync"))
        );
    }
}
