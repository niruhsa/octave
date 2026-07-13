//! Pure deterministic effective-profile resolver.

use super::model::*;

pub struct ResolveInput<'a> {
    pub preferences: &'a LocalPreferences,
    pub support_state: SupportState,
    pub synced: &'a EqualizerState,
    pub local: &'a LocalEqualizerState,
    pub manual_override: Option<&'a ProfileTarget>,
    pub exact_bindings: &'a [ExactBinding],
    pub outputs: &'a [AudioOutput],
    pub scope_epoch: Revision,
    pub resolution_generation: Revision,
}

pub fn resolve(input: ResolveInput<'_>) -> ResolvedEqualizer {
    let active_layer = if input.support_state == SupportState::Supported {
        ProfileLayer::Synced
    } else {
        ProfileLayer::LocalOnly
    };
    let state_revision = input.synced.state_revision;
    let selected = input.outputs.iter().find(|output| output.selected);
    let summary = selected.map(AudioOutputSummary::from);

    let result = if input.support_state == SupportState::FutureFormat {
        Resolution::flat(ResolveReason::Unsupported)
    } else if !input.preferences.master_enabled {
        Resolution::flat(ResolveReason::Disabled)
    } else if let Some(target) = input.manual_override {
        resolve_target(target, input.synced, input.local, ResolveReason::Manual)
    } else if input.preferences.automatic_switching_enabled {
        resolve_automatic(&input, active_layer, selected)
            .unwrap_or_else(|| resolve_default(active_layer, input.synced, input.local))
    } else {
        resolve_default(active_layer, input.synced, input.local)
    };

    ResolvedEqualizer {
        profile: result.profile,
        layer: result.layer,
        reason: result.reason,
        output_summary: result.output_summary.or(summary),
        state_revision,
        scope_epoch: input.scope_epoch,
        resolution_generation: input.resolution_generation,
    }
}

fn resolve_automatic(
    input: &ResolveInput<'_>,
    active_layer: ProfileLayer,
    selected: Option<&AudioOutput>,
) -> Option<Resolution> {
    if let Some(output) = selected {
        let active_is_reliable = matches!(
            output.accuracy,
            RouteAccuracy::Exact | RouteAccuracy::Predicted | RouteAccuracy::Default
        );
        if active_is_reliable {
            if let Some(key) = output.local_endpoint_key.as_deref() {
                if let Some(binding) = input
                    .exact_bindings
                    .iter()
                    .find(|binding| binding.endpoint_key == key && !binding.orphaned)
                {
                    let mut resolved = resolve_target(
                        &binding.target,
                        input.synced,
                        input.local,
                        ResolveReason::LocalExact,
                    );
                    resolved.output_summary = Some(AudioOutputSummary::from(output));
                    return Some(resolved);
                }
            }

            let rules = rules_for_layer(active_layer, input.synced, input.local);
            if let Some(matched) = best_rule(rules, output, TriggerKind::ActiveOutput) {
                let mut resolved = resolve_rule(
                    active_layer,
                    matched.rule,
                    input.synced,
                    input.local,
                    ResolveReason::PortableRule,
                );
                resolved.output_summary = Some(AudioOutputSummary::from(output));
                return Some(resolved);
            }
        }

        let rules = rules_for_layer(active_layer, input.synced, input.local);
        if matches!(
            output.accuracy,
            RouteAccuracy::ConnectedOnly | RouteAccuracy::Unavailable
        ) {
            if let Some((matched, candidate)) = input
                .outputs
                .iter()
                .filter(|candidate| candidate.connected)
                .filter_map(|candidate| {
                    best_rule(rules, candidate, TriggerKind::Connected)
                        .map(|matched| (matched, candidate))
                })
                .max_by(|(left, _), (right, _)| compare_matched(left, right))
            {
                let mut resolved = resolve_rule(
                    active_layer,
                    matched.rule,
                    input.synced,
                    input.local,
                    ResolveReason::ConnectedFallback,
                );
                resolved.output_summary = Some(AudioOutputSummary::from(candidate));
                return Some(resolved);
            }
        }
    } else {
        let rules = rules_for_layer(active_layer, input.synced, input.local);
        if let Some((matched, candidate)) = input
            .outputs
            .iter()
            .filter(|candidate| candidate.connected)
            .filter_map(|candidate| {
                best_rule(rules, candidate, TriggerKind::Connected)
                    .map(|matched| (matched, candidate))
            })
            .max_by(|(left, _), (right, _)| compare_matched(left, right))
        {
            let mut resolved = resolve_rule(
                active_layer,
                matched.rule,
                input.synced,
                input.local,
                ResolveReason::ConnectedFallback,
            );
            resolved.output_summary = Some(AudioOutputSummary::from(candidate));
            return Some(resolved);
        }
    }
    None
}

fn resolve_default(
    layer: ProfileLayer,
    synced: &EqualizerState,
    local: &LocalEqualizerState,
) -> Resolution {
    let id = match layer {
        ProfileLayer::Synced => synced.default_profile_id.as_deref(),
        ProfileLayer::LocalOnly => local.default_profile_id.as_deref(),
    };
    id.map_or_else(
        || Resolution::flat(ResolveReason::Flat),
        |profile_id| {
            resolve_target(
                &ProfileTarget {
                    layer,
                    action: RuleAction::Profile {
                        profile_id: profile_id.to_string(),
                    },
                },
                synced,
                local,
                ResolveReason::Default,
            )
        },
    )
}

fn resolve_rule(
    layer: ProfileLayer,
    rule: &EqualizerDeviceRule,
    synced: &EqualizerState,
    local: &LocalEqualizerState,
    reason: ResolveReason,
) -> Resolution {
    resolve_target(
        &ProfileTarget {
            layer,
            action: rule.action.clone(),
        },
        synced,
        local,
        reason,
    )
}

fn resolve_target(
    target: &ProfileTarget,
    synced: &EqualizerState,
    local: &LocalEqualizerState,
    reason: ResolveReason,
) -> Resolution {
    let RuleAction::Profile { profile_id } = &target.action else {
        return Resolution::flat(reason);
    };
    let profile = match target.layer {
        ProfileLayer::Synced => synced.profiles.iter().find(|row| row.id == *profile_id),
        ProfileLayer::LocalOnly => local.profiles.iter().find(|row| row.id == *profile_id),
    };
    profile.map_or_else(
        || Resolution::flat(ResolveReason::Flat),
        |profile| Resolution {
            profile: Some(profile.clone()),
            layer: Some(target.layer),
            reason,
            output_summary: None,
        },
    )
}

fn rules_for_layer<'a>(
    layer: ProfileLayer,
    synced: &'a EqualizerState,
    local: &'a LocalEqualizerState,
) -> &'a [EqualizerDeviceRule] {
    match layer {
        ProfileLayer::Synced => &synced.device_rules,
        ProfileLayer::LocalOnly => &local.device_rules,
    }
}

#[derive(Clone, Copy)]
struct MatchedRule<'a> {
    rule: &'a EqualizerDeviceRule,
    specificity: u8,
}

fn best_rule<'a>(
    rules: &'a [EqualizerDeviceRule],
    output: &AudioOutput,
    trigger: TriggerKind,
) -> Option<MatchedRule<'a>> {
    rules
        .iter()
        .filter(|rule| rule.enabled)
        .filter_map(|rule| {
            rule.selectors
                .iter()
                .filter(|selector| {
                    selector.trigger == trigger && selector_matches(selector, output)
                })
                .map(selector_specificity)
                .max()
                .map(|specificity| MatchedRule { rule, specificity })
        })
        .max_by(compare_matched)
}

fn compare_matched(left: &MatchedRule<'_>, right: &MatchedRule<'_>) -> std::cmp::Ordering {
    left.rule
        .priority
        .cmp(&right.rule.priority)
        .then_with(|| left.specificity.cmp(&right.specificity))
        // max_by should choose lexicographically smallest UUID on a tie.
        .then_with(|| right.rule.id.cmp(&left.rule.id))
}

fn selector_specificity(selector: &PortableDeviceSelector) -> u8 {
    match (
        selector.vendor_id.is_some() && selector.product_id.is_some(),
        !selector.normalized_name.is_empty(),
    ) {
        (true, true) => 3,
        (false, true) => 2,
        _ => 1,
    }
}

fn selector_matches(selector: &PortableDeviceSelector, output: &AudioOutput) -> bool {
    if selector.normalization_version != EQ_NORMALIZATION_VERSION
        || selector.route_kind != output.route_kind
        || selector.normalized_name != normalize_matcher(&output.display_name)
    {
        return false;
    }
    if let Some(platform) = selector.platform_scope {
        if Some(platform) != current_platform() {
            return false;
        }
    }
    let output_vendor = normalize_hardware_id(output.vendor_id.as_deref());
    let output_product = normalize_hardware_id(output.product_id.as_deref());
    selector
        .vendor_id
        .as_deref()
        .is_none_or(|value| normalize_hardware_id(Some(value)) == output_vendor)
        && selector
            .product_id
            .as_deref()
            .is_none_or(|value| normalize_hardware_id(Some(value)) == output_product)
}

fn current_platform() -> Option<Platform> {
    if cfg!(target_os = "windows") {
        Some(Platform::Windows)
    } else if cfg!(target_os = "android") {
        Some(Platform::Android)
    } else if cfg!(target_os = "macos") {
        Some(Platform::Macos)
    } else if cfg!(target_os = "linux") {
        Some(Platform::Linux)
    } else if cfg!(target_os = "ios") {
        Some(Platform::Ios)
    } else {
        None
    }
}

struct Resolution {
    profile: Option<EqualizerProfile>,
    layer: Option<ProfileLayer>,
    reason: ResolveReason,
    output_summary: Option<AudioOutputSummary>,
}

impl Resolution {
    fn flat(reason: ResolveReason) -> Self {
        Self {
            profile: None,
            layer: None,
            reason,
            output_summary: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> EqualizerProfile {
        EqualizerProfile::five_band_starter(name)
    }

    fn output(key: Option<&str>) -> AudioOutput {
        AudioOutput {
            runtime_id: None,
            local_endpoint_key: key.map(str::to_string),
            display_name: "Sony XM5".into(),
            route_kind: RouteKind::Bluetooth,
            vendor_id: None,
            product_id: None,
            connected: true,
            selected: true,
            accuracy: RouteAccuracy::Predicted,
            binding_stability: BindingStability::PersistentExact,
        }
    }

    fn selector(trigger: TriggerKind, name: &str) -> PortableDeviceSelector {
        PortableDeviceSelector {
            normalization_version: EQ_NORMALIZATION_VERSION,
            route_kind: RouteKind::Bluetooth,
            normalized_name: normalize_matcher(name),
            vendor_id: None,
            product_id: None,
            platform_scope: None,
            trigger,
        }
    }

    fn rule(
        id: &str,
        priority: i32,
        selectors: Vec<PortableDeviceSelector>,
    ) -> EqualizerDeviceRule {
        EqualizerDeviceRule {
            id: id.into(),
            label: id.into(),
            action: RuleAction::Bypass,
            selectors,
            priority,
            enabled: true,
            revision: Revision(1),
        }
    }

    #[test]
    fn automatic_toggle_gates_exact_bindings() {
        let p = profile("Default");
        let mut synced = EqualizerState {
            default_profile_id: Some(p.id.clone()),
            ..EqualizerState::default()
        };
        synced.profiles.push(p.clone());
        let bindings = vec![ExactBinding {
            endpoint_key: "key".into(),
            display_label: None,
            target: ProfileTarget {
                layer: ProfileLayer::Synced,
                action: RuleAction::Bypass,
            },
            orphaned: false,
        }];
        let outputs = vec![output(Some("key"))];
        let resolved = resolve(ResolveInput {
            preferences: &LocalPreferences {
                master_enabled: true,
                automatic_switching_enabled: false,
            },
            support_state: SupportState::Supported,
            synced: &synced,
            local: &LocalEqualizerState::default(),
            manual_override: None,
            exact_bindings: &bindings,
            outputs: &outputs,
            scope_epoch: Revision(1),
            resolution_generation: Revision(1),
        });
        assert_eq!(resolved.reason, ResolveReason::Default);
        assert_eq!(resolved.profile.unwrap().id, p.id);
    }

    #[test]
    fn exact_binding_precedes_portable_and_default() {
        let p = profile("Default");
        let mut synced = EqualizerState {
            default_profile_id: Some(p.id.clone()),
            ..EqualizerState::default()
        };
        synced.profiles.push(p);
        let bindings = vec![ExactBinding {
            endpoint_key: "key".into(),
            display_label: None,
            target: ProfileTarget {
                layer: ProfileLayer::Synced,
                action: RuleAction::Bypass,
            },
            orphaned: false,
        }];
        let outputs = vec![output(Some("key"))];
        let resolved = resolve(ResolveInput {
            preferences: &LocalPreferences {
                master_enabled: true,
                automatic_switching_enabled: true,
            },
            support_state: SupportState::Supported,
            synced: &synced,
            local: &LocalEqualizerState::default(),
            manual_override: None,
            exact_bindings: &bindings,
            outputs: &outputs,
            scope_epoch: Revision(1),
            resolution_generation: Revision(1),
        });
        assert_eq!(resolved.reason, ResolveReason::LocalExact);
        assert!(resolved.profile.is_none());
    }

    #[test]
    fn unreliable_selected_output_does_not_run_active_output_rules() {
        let mut selected = output(None);
        selected.accuracy = RouteAccuracy::ConnectedOnly;
        let mut synced = EqualizerState::default();
        synced.device_rules.push(rule(
            "active",
            100,
            vec![selector(TriggerKind::ActiveOutput, "Sony XM5")],
        ));
        let outputs = vec![selected];
        let resolved = resolve(ResolveInput {
            preferences: &LocalPreferences {
                master_enabled: true,
                automatic_switching_enabled: true,
            },
            support_state: SupportState::Supported,
            synced: &synced,
            local: &LocalEqualizerState::default(),
            manual_override: None,
            exact_bindings: &[],
            outputs: &outputs,
            scope_epoch: Revision(1),
            resolution_generation: Revision(1),
        });
        assert_eq!(resolved.reason, ResolveReason::Flat);
    }

    #[test]
    fn specificity_uses_best_matching_selector_not_an_unrelated_alias() {
        let generic = rule(
            "generic",
            5,
            vec![
                PortableDeviceSelector {
                    vendor_id: Some("1111".into()),
                    product_id: Some("2222".into()),
                    normalized_name: normalize_matcher("Different Device"),
                    ..selector(TriggerKind::ActiveOutput, "Different Device")
                },
                selector(TriggerKind::ActiveOutput, "Sony XM5"),
            ],
        );
        let specific = rule(
            "specific",
            5,
            vec![PortableDeviceSelector {
                vendor_id: Some("abcd".into()),
                product_id: Some("1234".into()),
                ..selector(TriggerKind::ActiveOutput, "Sony XM5")
            }],
        );
        let mut current = output(None);
        current.vendor_id = Some("ABCD".into());
        current.product_id = Some("0x1234".into());
        let rules = [generic, specific];
        let matched = best_rule(&rules, &current, TriggerKind::ActiveOutput).unwrap();
        assert_eq!(matched.rule.id, "specific");
    }

    #[test]
    fn connected_fallback_reports_the_candidate_that_matched() {
        let mut unavailable = output(None);
        unavailable.selected = false;
        unavailable.accuracy = RouteAccuracy::Unavailable;
        let mut candidate = output(None);
        candidate.selected = false;
        candidate.display_name = "Kitchen Speaker".into();
        candidate.accuracy = RouteAccuracy::ConnectedOnly;
        let mut synced = EqualizerState::default();
        synced.device_rules.push(rule(
            "speaker",
            1,
            vec![selector(TriggerKind::Connected, "Kitchen Speaker")],
        ));
        let outputs = vec![unavailable, candidate];
        let resolved = resolve(ResolveInput {
            preferences: &LocalPreferences {
                master_enabled: true,
                automatic_switching_enabled: true,
            },
            support_state: SupportState::Supported,
            synced: &synced,
            local: &LocalEqualizerState::default(),
            manual_override: None,
            exact_bindings: &[],
            outputs: &outputs,
            scope_epoch: Revision(1),
            resolution_generation: Revision(1),
        });
        assert_eq!(resolved.reason, ResolveReason::ConnectedFallback);
        assert_eq!(
            resolved.output_summary.unwrap().display_name,
            "Kitchen Speaker"
        );
    }
}
