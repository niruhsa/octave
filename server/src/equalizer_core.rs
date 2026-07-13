//! Pure canonicalization shared by the equalizer service and Postgres
//! rollback adapter. These algorithms are versioned persistence semantics.

use std::collections::HashSet;
use unicode_casefold::{Locale, UnicodeCaseFold, Variant};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::db::models::{EqualizerRuleAction, EqualizerState};

/// NFKC -> full non-Turkic default case fold -> NFKC.
pub(crate) fn canonical_casefold(value: &str) -> String {
    let nfkc = value.nfkc().collect::<String>();
    let folded = nfkc
        .as_str()
        .case_fold_with(Variant::Full, Locale::NonTurkic)
        .collect::<String>();
    folded.nfkc().collect()
}

/// Canonical unique key for a validated, already-trimmed profile name.
pub(crate) fn profile_name_key(value: &str) -> String {
    canonical_casefold(value)
}

/// Version-1 portable product matcher normalization. Whitespace collapsing is
/// deliberately after the second NFKC pass.
pub(crate) fn normalize_device_name(value: &str) -> String {
    canonical_casefold(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rows/references whose content changed across one audit envelope. Aggregate
/// `state_revision` is intentionally excluded: unrelated later work may bump
/// it without making this inverse unsafe.
#[derive(Debug, Clone)]
pub(crate) struct RollbackDiff {
    pub profile_ids: HashSet<Uuid>,
    pub rule_ids: HashSet<Uuid>,
    pub settings: bool,
}

pub(crate) fn rollback_diff(before: &EqualizerState, after: &EqualizerState) -> RollbackDiff {
    let profile_ids = before
        .profiles
        .iter()
        .chain(after.profiles.iter())
        .map(|p| p.id)
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|id| {
            before.profiles.iter().find(|p| p.id == *id)
                != after.profiles.iter().find(|p| p.id == *id)
        })
        .collect();
    let rule_ids = before
        .device_rules
        .iter()
        .chain(after.device_rules.iter())
        .map(|r| r.id)
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|id| {
            before.device_rules.iter().find(|r| r.id == *id)
                != after.device_rules.iter().find(|r| r.id == *id)
        })
        .collect();
    RollbackDiff {
        profile_ids,
        rule_ids,
        settings: before.default_profile_id != after.default_profile_id
            || before.settings_revision != after.settings_revision,
    }
}

/// Verify only the rows and references touched by the original mutation.
/// Unrelated work is preserved by [`apply_rollback_inverse`].
pub(crate) fn rollback_after_image_matches(
    current: &EqualizerState,
    after: &EqualizerState,
    diff: &RollbackDiff,
) -> bool {
    if current.state_format_version != after.state_format_version {
        return false;
    }
    if diff.profile_ids.iter().any(|id| {
        current.profiles.iter().find(|p| p.id == *id) != after.profiles.iter().find(|p| p.id == *id)
    }) || diff.rule_ids.iter().any(|id| {
        current.device_rules.iter().find(|r| r.id == *id)
            != after.device_rules.iter().find(|r| r.id == *id)
    }) {
        return false;
    }
    if diff.settings
        && (current.default_profile_id != after.default_profile_id
            || current.settings_revision != after.settings_revision)
    {
        return false;
    }
    // A later rule/default assignment to a profile that this inverse changes
    // is itself a relevant reference, even though that row was not in the
    // original diff. Do not silently change its behavior or violate an FK.
    diff.profile_ids
        .iter()
        .all(|id| profile_references(current, *id) == profile_references(after, *id))
}

/// Overlay the historical before-image only for affected rows, leaving every
/// unrelated current profile/rule/settings value intact.
pub(crate) fn apply_rollback_inverse(
    current: &EqualizerState,
    before: &EqualizerState,
    diff: &RollbackDiff,
) -> EqualizerState {
    let mut target = current.clone();
    target
        .profiles
        .retain(|p| !diff.profile_ids.contains(&p.id));
    target.profiles.extend(
        before
            .profiles
            .iter()
            .filter(|p| diff.profile_ids.contains(&p.id))
            .cloned(),
    );
    target
        .device_rules
        .retain(|r| !diff.rule_ids.contains(&r.id));
    target.device_rules.extend(
        before
            .device_rules
            .iter()
            .filter(|r| diff.rule_ids.contains(&r.id))
            .cloned(),
    );
    if diff.settings {
        target.default_profile_id = before.default_profile_id;
    }
    target.profiles.sort_by(|a, b| {
        profile_name_key(&a.name)
            .cmp(&profile_name_key(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    target
        .device_rules
        .sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
    target
}

fn profile_references(state: &EqualizerState, profile_id: Uuid) -> (bool, Vec<(Uuid, i64)>) {
    let mut rules = state
        .device_rules
        .iter()
        .filter_map(|rule| match rule.action {
            EqualizerRuleAction::Profile { profile_id: id } if id == profile_id => {
                Some((rule.id, rule.revision))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    rules.sort_unstable();
    (state.default_profile_id == Some(profile_id), rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_case_fold_and_nfkc_are_stable() {
        assert_eq!(canonical_casefold("Straße"), "strasse");
        assert_eq!(canonical_casefold("ＳＯＮＹ"), "sony");
    }

    #[test]
    fn matcher_collapses_unicode_whitespace() {
        assert_eq!(normalize_device_name("  Sony\u{2003}XM5  "), "sony xm5");
    }
}
