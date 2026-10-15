//! Port of upstream/apps/web/src/components/pullRequest/pullRequestEditing.logic.ts,
//! plus the pure state machine from useInlineActionConfirm.ts and the item model from
//! pullRequestLinkContextMenu.ts. React hooks, DOM events, timers, clipboard, host menu,
//! and toast handling are skipped.

use turin_contracts::js_trim;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestActor, PullRequestCapabilities, PullRequestComment,
    PullRequestCommentKind, PullRequestViewerPermissions,
};

pub struct EditingSubject<'a> {
    pub author: Option<&'a PullRequestActor>,
    pub capabilities: &'a PullRequestCapabilities,
    pub viewer: Option<&'a str>,
    pub viewer_permissions: &'a PullRequestViewerPermissions,
}
fn same_login(one: Option<&str>, other: Option<&str>) -> bool {
    match (one, other) {
        (Some(a), Some(b)) => js_trim(a).to_lowercase() == js_trim(b).to_lowercase(),
        _ => false,
    }
}
/// Host permission wins; otherwise authorship or merge permission stands in for write access.
pub fn can_edit_pull_request_change_request(detail: &EditingSubject<'_>) -> bool {
    if !detail
        .capabilities
        .edit
        .as_ref()
        .is_some_and(|edit| edit.change_request)
    {
        return false;
    }
    if let Some(answer) = detail.viewer_permissions.edit_change_request {
        return answer;
    }
    same_login(
        detail.viewer,
        detail.author.map(|author| author.login.as_str()),
    ) || detail
        .viewer_permissions
        .actions
        .contains(&PullRequestAction::Merge)
}
/// A review summary is never rewritten via the comment mutation.
pub fn can_edit_pull_request_comment(
    detail: &EditingSubject<'_>,
    comment: &PullRequestComment,
) -> bool {
    if !detail
        .capabilities
        .edit
        .as_ref()
        .is_some_and(|edit| edit.comment)
    {
        return false;
    }
    if !matches!(
        comment.kind,
        PullRequestCommentKind::IssueComment | PullRequestCommentKind::ReviewComment
    ) {
        return false;
    }
    comment.can_edit.unwrap_or_else(|| {
        same_login(
            detail.viewer,
            comment.author.as_ref().map(|author| author.login.as_str()),
        )
    })
}
/// Per-thread permission can only narrow the repository-wide permission.
pub fn can_resolve_pull_request_thread(
    viewer_permissions: &PullRequestViewerPermissions,
    can_resolve: Option<bool>,
) -> bool {
    viewer_permissions.resolve && can_resolve != Some(false)
}

/// One action and one rendered control can be armed at a time, even across menu portals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineActionConfirm {
    pub scope_key: String,
    pub action: String,
    pub element_key: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmClick {
    Armed,
    Confirmed,
}
impl InlineActionConfirm {
    pub fn is_armed(&self, scope_key: &str, action: &str) -> bool {
        self.scope_key == scope_key && self.action == action
    }
    /// A second click confirms only when it hits the same rendered element.
    pub fn click(
        armed: &mut Option<Self>,
        scope_key: &str,
        action: &str,
        element_key: &str,
    ) -> ConfirmClick {
        if armed.as_ref().is_some_and(|active| {
            active.is_armed(scope_key, action) && active.element_key == element_key
        }) {
            *armed = None;
            ConfirmClick::Confirmed
        } else {
            *armed = Some(Self {
                scope_key: scope_key.to_owned(),
                action: action.to_owned(),
                element_key: element_key.to_owned(),
            });
            ConfirmClick::Armed
        }
    }
    pub fn disarm(armed: &mut Option<Self>) {
        *armed = None;
    }
    pub fn scope_changed(armed: &mut Option<Self>, scope_key: &str) {
        if armed
            .as_ref()
            .is_some_and(|active| active.scope_key != scope_key)
        {
            *armed = None;
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequestLinkContextMenuAction {
    CopyLink,
    OpenExternal,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestLinkContextMenuItem {
    pub id: PullRequestLinkContextMenuAction,
    pub label: String,
    pub icon: Option<&'static str>,
}
/// Name the host where the reader will land.
pub fn open_on_host_label(host_label: Option<&str>) -> String {
    host_label.map_or_else(
        || "Open on host".to_owned(),
        |label| format!("Open on {label}"),
    )
}
/// Copy first: it is why a reader right-clicks the number.
pub fn pull_request_link_context_menu_items(
    open_label: &str,
) -> Vec<PullRequestLinkContextMenuItem> {
    vec![
        PullRequestLinkContextMenuItem {
            id: PullRequestLinkContextMenuAction::CopyLink,
            label: "Copy link".to_owned(),
            icon: Some("copy"),
        },
        PullRequestLinkContextMenuItem {
            id: PullRequestLinkContextMenuAction::OpenExternal,
            label: open_label.to_owned(),
            icon: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    fn actor(login: &str) -> PullRequestActor {
        serde_json::from_value(json!({"login":login,"name":null,"avatarUrl":null})).unwrap()
    }
    fn caps(change: Option<bool>, comment: Option<bool>) -> PullRequestCapabilities {
        let mut value = json!({"diff":true,"comment":true,"actions":[],"mergeMethods":[],"search":true,"review":{"inlineComment":true,"reply":true,"resolve":true,"verdicts":[]},"reviewers":{"request":true,"listCandidates":true}});
        if let (Some(change), Some(comment)) = (change, comment) {
            value["edit"] = json!({"changeRequest":change,"comment":comment});
        }
        serde_json::from_value(value).unwrap()
    }
    fn perms(actions: &[&str], resolve: bool, edit: Option<bool>) -> PullRequestViewerPermissions {
        let mut value = json!({"actions":actions,"comment":true,"resolve":resolve,"verdicts":[],"requestReviewers":true});
        if let Some(edit) = edit {
            value["editChangeRequest"] = json!(edit);
        }
        serde_json::from_value(value).unwrap()
    }
    fn remark(kind: &str, author: Option<&str>, can_edit: Option<bool>) -> PullRequestComment {
        let mut value = json!({"id":"c1","kind":kind,"author":author.map(|name|json!({"login":name,"name":null,"avatarUrl":null})),"body":"words","createdAt":"2026-01-01T00:00:00.000Z","url":null,"path":null,"reviewState":null});
        if let Some(can_edit) = can_edit {
            value["canEdit"] = json!(can_edit);
        }
        serde_json::from_value(value).unwrap()
    }
    fn edit(
        author: Option<&str>,
        viewer: Option<&str>,
        change: Option<bool>,
        comment: Option<bool>,
        actions: &[&str],
        host_edit: Option<bool>,
        remark: Option<&PullRequestComment>,
    ) -> bool {
        let author = author.map(actor);
        let caps = caps(change, comment);
        let perms = perms(actions, true, host_edit);
        let subject = EditingSubject {
            author: author.as_ref(),
            capabilities: &caps,
            viewer,
            viewer_permissions: &perms,
        };
        remark.map_or_else(
            || can_edit_pull_request_change_request(&subject),
            |r| can_edit_pull_request_comment(&subject, r),
        )
    }
    #[test]
    fn author_can_rewrite() {
        assert!(edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(true),
            &[],
            None,
            None
        ));
    }
    #[test]
    fn author_case_insensitive() {
        assert!(edit(
            Some("octocat"),
            Some("OctoCat"),
            Some(true),
            Some(true),
            &[],
            None,
            None
        ));
    }
    #[test]
    fn merger_can_rewrite() {
        assert!(edit(
            Some("someone-else"),
            Some("octocat"),
            Some(true),
            Some(true),
            &["merge"],
            None,
            None
        ));
    }
    #[test]
    fn host_answer_overrides_author_and_merge() {
        assert!(edit(
            Some("someone-else"),
            Some("octocat"),
            Some(true),
            Some(true),
            &[],
            Some(true),
            None
        ));
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(true),
            &["merge"],
            Some(false),
            None
        ));
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            Some(false),
            Some(true),
            &[],
            Some(true),
            None
        ));
    }
    #[test]
    fn neither_author_nor_merger() {
        assert!(!edit(
            Some("someone-else"),
            Some("octocat"),
            Some(true),
            Some(true),
            &["close"],
            None,
            None
        ));
    }
    #[test]
    fn host_cannot_rewrite_change_request() {
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            Some(false),
            Some(true),
            &[],
            None,
            None
        ));
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            None,
            None,
            &[],
            None,
            None
        ));
    }
    #[test]
    fn unnamed_viewer_cannot_rewrite() {
        assert!(!edit(
            Some("octocat"),
            None,
            Some(true),
            Some(true),
            &[],
            None,
            None
        ));
    }
    #[test]
    fn unnamed_merger_can_rewrite() {
        assert!(edit(
            Some("octocat"),
            None,
            Some(true),
            Some(true),
            &["merge"],
            None,
            None
        ));
    }
    #[test]
    fn author_can_edit_issue_comment() {
        assert!(edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(true),
            &[],
            None,
            Some(&remark("issue-comment", Some("octocat"), None))
        ));
    }
    #[test]
    fn author_can_edit_line_remark() {
        assert!(edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(true),
            &[],
            None,
            Some(&remark("review-comment", Some("octocat"), None))
        ));
    }
    #[test]
    fn comment_author_case_insensitive() {
        assert!(edit(
            Some("octocat"),
            Some("OCTOCAT"),
            Some(true),
            Some(true),
            &[],
            None,
            Some(&remark("issue-comment", Some("octocat"), None))
        ));
    }
    #[test]
    fn other_persons_comment() {
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(true),
            &[],
            None,
            Some(&remark("issue-comment", Some("someone-else"), None))
        ));
    }
    #[test]
    fn host_comment_answer_overrides_authorship() {
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(true),
            &[],
            None,
            Some(&remark("issue-comment", Some("octocat"), Some(false)))
        ));
        assert!(edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(true),
            &[],
            None,
            Some(&remark("issue-comment", Some("someone-else"), Some(true)))
        ));
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(false),
            &[],
            None,
            Some(&remark("issue-comment", Some("octocat"), Some(true)))
        ));
    }
    #[test]
    fn unattributed_remark() {
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(true),
            &[],
            None,
            Some(&remark("issue-comment", None, None))
        ));
    }
    #[test]
    fn review_summary_is_not_comment() {
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(true),
            &[],
            None,
            Some(&remark("review", Some("octocat"), None))
        ));
    }
    #[test]
    fn host_cannot_rewrite_comment() {
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            Some(true),
            Some(false),
            &[],
            None,
            Some(&remark("issue-comment", Some("octocat"), None))
        ));
        assert!(!edit(
            Some("octocat"),
            Some("octocat"),
            None,
            None,
            &[],
            None,
            Some(&remark("issue-comment", Some("octocat"), None))
        ));
    }
    #[test]
    fn unnamed_viewer_cannot_rewrite_comment() {
        assert!(!edit(
            Some("octocat"),
            None,
            Some(true),
            Some(true),
            &[],
            None,
            Some(&remark("issue-comment", Some("octocat"), None))
        ));
    }
    #[test]
    fn repository_permission_when_thread_silent() {
        assert!(can_resolve_pull_request_thread(
            &perms(&[], true, None),
            None
        ));
        assert!(!can_resolve_pull_request_thread(
            &perms(&[], false, None),
            None
        ));
    }
    #[test]
    fn thread_only_narrows_permission() {
        assert!(!can_resolve_pull_request_thread(
            &perms(&[], true, None),
            Some(false)
        ));
        assert!(!can_resolve_pull_request_thread(
            &perms(&[], false, None),
            Some(true)
        ));
    }
    #[test]
    fn inline_confirm_requires_second_click_on_same_element() {
        let mut armed = None;
        assert_eq!(
            InlineActionConfirm::click(&mut armed, "scope", "delete", "button"),
            ConfirmClick::Armed
        );
        assert_eq!(
            InlineActionConfirm::click(&mut armed, "scope", "delete", "other"),
            ConfirmClick::Armed
        );
        assert_eq!(
            InlineActionConfirm::click(&mut armed, "scope", "delete", "other"),
            ConfirmClick::Confirmed
        );
        assert!(armed.is_none());
    }
    #[test]
    fn inline_confirm_disarms_on_scope_change() {
        let mut armed = None;
        InlineActionConfirm::click(&mut armed, "old", "delete", "button");
        InlineActionConfirm::scope_changed(&mut armed, "new");
        assert!(armed.is_none());
    }
    #[test]
    fn context_menu_copy_first_and_host_named() {
        assert_eq!(open_on_host_label(None), "Open on host");
        assert_eq!(open_on_host_label(Some("GitHub")), "Open on GitHub");
        let items = pull_request_link_context_menu_items("Open on GitHub");
        assert_eq!(items[0].id, PullRequestLinkContextMenuAction::CopyLink);
        assert_eq!(items[1].id, PullRequestLinkContextMenuAction::OpenExternal);
    }
}
