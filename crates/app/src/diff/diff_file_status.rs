//! Port of upstream/apps/web/src/components/diffs/DiffFileStatus.tsx

/// Tooltip and aria label for the diff file header status control.
pub struct DiffFileStatusLabels {
    pub aria_label: &'static str,
    pub tooltip: &'static str,
    pub retry_on_click: bool,
}

/// Returns labels when `error` or `truncated` is set; otherwise the status is hidden upstream.
pub fn diff_file_status_labels(error: bool, truncated: bool) -> Option<DiffFileStatusLabels> {
    if !error && !truncated {
        return None;
    }
    Some(if error {
        DiffFileStatusLabels {
            aria_label: "Retry loading diff",
            tooltip: "Retry loading diff",
            retry_on_click: true,
        }
    } else {
        DiffFileStatusLabels {
            aria_label: "Partial diff preview",
            tooltip: "This file is too large to show in full. Counts include all changes.",
            retry_on_click: false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_when_healthy() {
        assert!(diff_file_status_labels(false, false).is_none());
    }

    #[test]
    fn retry_when_error() {
        let labels = diff_file_status_labels(true, false).unwrap();
        assert!(labels.retry_on_click);
        assert_eq!(labels.tooltip, "Retry loading diff");
    }

    #[test]
    fn info_when_truncated() {
        let labels = diff_file_status_labels(false, true).unwrap();
        assert!(!labels.retry_on_click);
        assert!(labels.tooltip.contains("too large"));
    }
}
