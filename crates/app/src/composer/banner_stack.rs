//! The ordering rule of upstream/apps/web/src/components/chat/ComposerBannerStack.tsx
//! (`bannerPriority` + the stable sort). The DOM-measured notice-details
//! disclosure its test covers is not ported: the view clips the description
//! with an ellipsis and offers its full text as a tooltip.

use crate::composer::view_model::{BannerItem, BannerPriority, BannerVariant};

/// Lower sorts first: activity stays attached to the composer, urgent or
/// severe notices come next, everything else last.
pub fn banner_priority(priority: Option<BannerPriority>, variant: BannerVariant) -> u8 {
    if priority == Some(BannerPriority::Activity) {
        return 0;
    }
    if priority == Some(BannerPriority::Urgent) || matches!(variant, BannerVariant::Error | BannerVariant::Warning) {
        return 1;
    }
    2
}

/// Stable order: the first item is the attached front banner, the rest stack
/// behind it.
pub fn order_banners(items: &[BannerItem]) -> Vec<&BannerItem> {
    let mut ordered: Vec<&BannerItem> = items.iter().collect();
    ordered.sort_by_key(|item| banner_priority(item.priority, item.variant));
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::IconName;

    fn item(id: &str, variant: BannerVariant, priority: Option<BannerPriority>) -> BannerItem {
        BannerItem {
            id: id.into(),
            variant,
            priority,
            icon: IconName::Info,
            title: id.into(),
            description: None,
            dismissible: false,
            dismiss_label: None,
            actions: Vec::new(),
        }
    }

    fn ids(items: &[BannerItem]) -> Vec<String> {
        order_banners(items).into_iter().map(|item| item.id.clone()).collect()
    }

    #[test]
    fn activity_stays_attached_in_front_of_urgent_and_severe_notices() {
        let items = [
            item("info", BannerVariant::Info, None),
            item("error", BannerVariant::Error, None),
            item("activity", BannerVariant::Default, Some(BannerPriority::Activity)),
            item("urgent", BannerVariant::Info, Some(BannerPriority::Urgent)),
        ];
        assert_eq!(ids(&items), ["activity", "error", "urgent", "info"]);
    }

    #[test]
    fn equal_priorities_keep_their_given_order() {
        let items = [
            item("a", BannerVariant::Success, None),
            item("b", BannerVariant::Info, Some(BannerPriority::Notice)),
            item("c", BannerVariant::Default, None),
        ];
        assert_eq!(ids(&items), ["a", "b", "c"]);
    }

    #[test]
    fn warnings_outrank_plain_notices() {
        assert_eq!(banner_priority(None, BannerVariant::Warning), 1);
        assert_eq!(banner_priority(None, BannerVariant::Info), 2);
        assert_eq!(banner_priority(Some(BannerPriority::Activity), BannerVariant::Error), 0);
    }
}
