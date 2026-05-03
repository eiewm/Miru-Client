use crate::osu::ResolvedBeatmap;
use crate::types::{AutoRendererConfig, CountRule, CountRuleOp, HitData};

#[derive(Debug, Clone)]
pub struct AutoRendererReplayMetrics {
    pub max_combo: f64,
    pub accuracy: f64,
    pub pp: Option<f64>,
    pub hits: HitData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoRendererFilterDecision {
    Match,
    Rejected,
    PpUnavailable,
}

pub fn matches_auto_renderer_filters(
    auto_renderer: &AutoRendererConfig,
    beatmap: &ResolvedBeatmap,
) -> AutoRendererFilterDecision {
    let key_count_matches = auto_renderer.key_counts.is_empty()
        || auto_renderer
            .key_counts
            .iter()
            .any(|value| *value == beatmap.key_count);

    if !key_count_matches {
        return AutoRendererFilterDecision::Rejected;
    }

    let map_rules_match = numeric_rule_matches(
        &auto_renderer.long_note_rule,
        f64::from(beatmap.long_note_count),
    ) && numeric_rule_matches(
        &auto_renderer.normal_note_rule,
        f64::from(beatmap.normal_note_count),
    ) && numeric_rule_matches(
        &auto_renderer.total_note_rule,
        f64::from(
            beatmap
                .long_note_count
                .saturating_add(beatmap.normal_note_count),
        ),
    ) && numeric_rule_matches(&auto_renderer.bpm_rule, beatmap.bpm)
        && numeric_rule_matches(&auto_renderer.hp_rule, beatmap.hp)
        && numeric_rule_matches(&auto_renderer.cs_rule, beatmap.cs)
        && numeric_rule_matches(&auto_renderer.od_rule, beatmap.od)
        && numeric_rule_matches(
            &auto_renderer.duration_rule,
            f64::from(beatmap.duration_ms) / 1000.0,
        );

    if map_rules_match {
        AutoRendererFilterDecision::Match
    } else {
        AutoRendererFilterDecision::Rejected
    }
}

pub fn matches_auto_renderer_filters_with_metrics(
    auto_renderer: &AutoRendererConfig,
    beatmap: &ResolvedBeatmap,
    metrics: &AutoRendererReplayMetrics,
) -> AutoRendererFilterDecision {
    if matches_auto_renderer_filters(auto_renderer, beatmap) == AutoRendererFilterDecision::Rejected
    {
        return AutoRendererFilterDecision::Rejected;
    }

    if auto_renderer.pp_rule.enabled && metrics.pp.is_none() {
        return AutoRendererFilterDecision::PpUnavailable;
    }

    let replay_rules_match = numeric_rule_matches(&auto_renderer.max_combo_rule, metrics.max_combo)
        && numeric_rule_matches(&auto_renderer.accuracy_rule, metrics.accuracy)
        && optional_numeric_rule_matches(&auto_renderer.pp_rule, metrics.pp)
        && numeric_rule_matches(&auto_renderer.judgment_rules.max, metrics.hits.geki as f64)
        && numeric_rule_matches(&auto_renderer.judgment_rules.n300, metrics.hits.n300 as f64)
        && numeric_rule_matches(&auto_renderer.judgment_rules.n200, metrics.hits.katu as f64)
        && numeric_rule_matches(&auto_renderer.judgment_rules.n100, metrics.hits.n100 as f64)
        && numeric_rule_matches(&auto_renderer.judgment_rules.n50, metrics.hits.n50 as f64)
        && numeric_rule_matches(&auto_renderer.judgment_rules.miss, metrics.hits.miss as f64);

    if replay_rules_match {
        AutoRendererFilterDecision::Match
    } else {
        AutoRendererFilterDecision::Rejected
    }
}

fn optional_numeric_rule_matches(rule: &CountRule, value: Option<f64>) -> bool {
    if !rule.enabled {
        return true;
    }

    value.is_some_and(|actual| numeric_rule_matches(rule, actual))
}

fn numeric_rule_matches(rule: &CountRule, value: f64) -> bool {
    if !rule.enabled {
        return true;
    }

    match rule.op {
        CountRuleOp::Eq => (value - rule.value).abs() < 0.001,
        CountRuleOp::Gte => value >= rule.value,
        CountRuleOp::Lte => value <= rule.value,
        CountRuleOp::Between => {
            let max = rule.max_value.unwrap_or(rule.value);
            let lower = rule.value.min(max);
            let upper = rule.value.max(max);
            value >= lower && value <= upper
        }
    }
}
