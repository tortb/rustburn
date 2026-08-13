//! 评分归一层（v2 架构）。
//!
//! 只消费五个 [DimensionResult] 做加权合成，**不包含任何维度的具体计算逻辑**，
//! 不读取 [crate::model::FileRawMetrics]（SPEC v2 §7 禁止事项 7-A）。
//!
//! 公式：
//! ```text
//! base_risk = w1×complexity_risk + w2×duplication_risk + w3×test_risk
//!           + w4×change_risk + w5×dependency_risk
//!           + 高风险维度惩罚
//! ```
//! 默认权重：complexity=0.30 duplication=0.15 test=0.25
//!          change_risk=0.20 dependency=0.10
//!
//! 任意维度 `NotApplicable` 时（§7 禁止事项 7-B）：该维度权重按比例分摊到其余
//! 维度上重新归一，不当作 0 分参与加权，并在结果中标注被排除的维度。

use crate::model::{DimensionResult, HistoricalSnapshot, HistoryRewriteState};

/// 五个维度固定顺序（与 [crate::model::DIMENSION_NAMES] 一致）。
pub const DIMENSION_NAMES: [&str; 5] = [
    "complexity",
    "duplication",
    "test",
    "change_risk",
    "dependency",
];

/// 默认权重（总和 = 1.0）。
pub const DEFAULT_WEIGHTS: [f64; 5] = [0.30, 0.15, 0.25, 0.20, 0.10];

/// 基础风险合成结果（含归一化明细，供报告展示）。
#[derive(Debug, Clone)]
pub struct BaseRiskComposition {
    /// 最终基础风险分（0-100）
    pub base_risk_score: f64,
    /// 高风险维度惩罚
    pub extra_penalty: f64,
    /// 参与加权的维度
    pub active_dimensions: Vec<&'static str>,
    /// 被排除（NotApplicable）的维度
    pub excluded_dimensions: Vec<&'static str>,
    /// 归一化后的权重（总和 = 1.0）
    pub normalized_weights: Vec<(&'static str, f64)>,
}

/// 计算基础风险分。
///
/// - 权重按默认权重，NotApplicable 维度排除后按比例重新归一；
/// - 高风险维度惩罚逻辑同原 SPEC：仅当某维度显著偏离其余维度均值时追加
///   `(max - mean_of_others) × 0.15`（max > 50 且 max > mean_of_others × 1.25）。
pub fn calculate_base_risk_score(dims: &[DimensionResult; 5]) -> BaseRiskComposition {
    let mut active: Vec<(&'static str, f64)> = Vec::new();
    let mut excluded: Vec<&'static str> = Vec::new();
    let mut total_w = 0.0;
    let mut weighted = 0.0;

    for (i, d) in dims.iter().enumerate() {
        if d.is_excluded() {
            excluded.push(DIMENSION_NAMES[i]);
        } else {
            active.push((DIMENSION_NAMES[i], d.risk_score));
            total_w += DEFAULT_WEIGHTS[i];
            weighted += DEFAULT_WEIGHTS[i] * d.risk_score;
        }
    }

    // 全部维度都被排除的极端情况
    if active.is_empty() || total_w <= 0.0 {
        return BaseRiskComposition {
            base_risk_score: 0.0,
            extra_penalty: 0.0,
            active_dimensions: Vec::new(),
            excluded_dimensions: excluded,
            normalized_weights: Vec::new(),
        };
    }

    // 权重按比例分摊：总和保持 1.0
    let normalized_weights: Vec<(&'static str, f64)> = active
        .iter()
        .map(|(name, _)| (*name, DEFAULT_WEIGHTS[dim_index(name)] / total_w))
        .collect();

    let base = weighted / total_w;
    let active_scores: Vec<f64> = active.iter().map(|(_, s)| *s).collect();
    let extra = extra_penalty(&active_scores);
    let result = (base + extra).clamp(0.0, 100.0);

    if crate::debug_enabled() {
        crate::debug_log(format_args!(
            "base_risk active={} excluded={:?} weighted={:.3} extra={:.3} -> base_risk={:.3}",
            active.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(","),
            excluded,
            base,
            extra,
            result
        ));
    }

    BaseRiskComposition {
        base_risk_score: result,
        extra_penalty: extra,
        active_dimensions: active.iter().map(|(n, _)| *n).collect(),
        excluded_dimensions: excluded,
        normalized_weights,
    }
}

fn dim_index(name: &str) -> usize {
    DIMENSION_NAMES.iter().position(|&n| n == name).unwrap_or(0)
}

/// 高风险维度惩罚：仅当最大维度显著偏离其余维度均值时才追加。
fn extra_penalty(scores: &[f64]) -> f64 {
    let n = scores.len();
    if n == 0 {
        return 0.0;
    }
    let max_dim = scores.iter().copied().fold(f64::MIN, f64::max);
    if n == 1 {
        // 只有一个维度参与：显著偏离无从谈起
        return 0.0;
    }
    let sum: f64 = scores.iter().sum();
    let mean_of_others = (sum - max_dim) / (n - 1) as f64;

    if max_dim > 50.0 && max_dim > mean_of_others * 1.25 {
        (max_dim - mean_of_others) * 0.15
    } else {
        0.0
    }
}

/// 计算一致性系数（仅用于置信度，不参与 final_heat_score）。
pub fn calculate_consistency_coefficient(
    coverage_report_stale: bool,
    history_rewrite: HistoryRewriteState,
    lockfile_mismatch: bool,
) -> f64 {
    let mut coefficient: f64 = 1.0;
    if history_rewrite == HistoryRewriteState::Detected {
        coefficient *= 0.7;
    }
    if coverage_report_stale {
        coefficient *= 0.85;
    }
    if lockfile_mismatch {
        coefficient *= 0.9;
    }
    coefficient.max(0.5)
}

/// 计算趋势系数。
pub fn calculate_trend_coefficient(snapshots: &[HistoricalSnapshot]) -> f64 {
    if snapshots.is_empty() {
        return 1.0;
    }
    let historical_mean: f64 =
        snapshots.iter().map(|s| s.base_risk_score).sum::<f64>() / snapshots.len() as f64;
    let current = snapshots.last().map(|s| s.base_risk_score).unwrap_or(0.0);
    let trend_delta = (historical_mean - current) / historical_mean.max(1.0);
    let trend_delta = trend_delta.clamp(-0.3, 0.3);
    (1.0 - trend_delta * 0.3).clamp(0.91, 1.09)
}

/// 计算最终热度分数：final_heat_score = base_risk_score × trend_coefficient。
pub fn calculate_final_heat_score(base_risk_score: f64, trend_coefficient: f64) -> f64 {
    (base_risk_score * trend_coefficient).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Confidence, DimensionResult};
    use serde_json::json;

    fn dim(risk: f64, confidence: Confidence) -> DimensionResult {
        DimensionResult {
            raw_value: risk,
            risk_score: risk,
            confidence,
            detail: json!({}),
        }
    }

    #[test]
    fn test_weighted_sum_all_active() {
        let dims = [
            dim(30.0, Confidence::Full),
            dim(40.0, Confidence::Full),
            dim(50.0, Confidence::Full),
            dim(60.0, Confidence::Full),
            dim(70.0, Confidence::Full),
        ];
        let c = calculate_base_risk_score(&dims);
        // weighted = 0.3*30 + 0.15*40 + 0.25*50 + 0.2*60 + 0.1*70 = 46.5
        // max=70 > 50 且 70 > mean_of_others(45)*1.25=56.25 → penalty=(70-45)*0.15=3.75
        // base = 46.5 + 3.75 = 50.25
        assert!(
            (c.base_risk_score - 50.25).abs() < 1e-9,
            "{}",
            c.base_risk_score
        );
        assert!(c.excluded_dimensions.is_empty());
        // 权重总和保持 1.0
        let wsum: f64 = c.normalized_weights.iter().map(|(_, w)| *w).sum();
        assert!((wsum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_not_applicable_renormalizes_weights() {
        // duplication NotApplicable（权重 0.15）→ 分摊到其余维度，权重总和仍为 1.0
        let dims = [
            dim(30.0, Confidence::Full),
            dim(40.0, Confidence::NotApplicable),
            dim(40.0, Confidence::Full),
            dim(30.0, Confidence::Full),
            dim(20.0, Confidence::Full),
        ];
        let c = calculate_base_risk_score(&dims);
        assert_eq!(c.excluded_dimensions, vec!["duplication"]);
        let wsum: f64 = c.normalized_weights.iter().map(|(_, w)| *w).sum();
        assert!(
            (wsum - 1.0).abs() < 1e-9,
            "权重分摊后总和必须为 1.0，实际 {}",
            wsum
        );
        // 剩余权重 0.85，加权值 = (9 + 10 + 6 + 2) / 0.85 = 31.7647（max=40 未触发惩罚）
        assert!((c.base_risk_score - 27.0 / 0.85).abs() < 1e-6);
    }

    #[test]
    fn test_penalty_only_on_significant_imbalance() {
        // 相对均衡时不追加惩罚（max 未超过其余均值 1.25 倍）
        let dims = [
            dim(80.0, Confidence::Full),
            dim(75.0, Confidence::Full),
            dim(70.0, Confidence::Full),
            dim(65.0, Confidence::Full),
            dim(60.0, Confidence::Full),
        ];
        let c = calculate_base_risk_score(&dims);
        // max=80，mean_of_others=67.5，67.5*1.25=84.4 → 80 < 84.4，无惩罚
        assert!((c.extra_penalty - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_penalty_on_single_high_dimension() {
        let dims = [
            dim(95.0, Confidence::Full),
            dim(5.0, Confidence::Full),
            dim(5.0, Confidence::Full),
            dim(5.0, Confidence::Full),
            dim(5.0, Confidence::Full),
        ];
        let c = calculate_base_risk_score(&dims);
        // max=95 > 50 且 > mean_of_others(5)*1.25=6.25 → penalty = (95-5)*0.15 = 13.5
        assert!((c.extra_penalty - 13.5).abs() < 1e-9);
        assert!(c.base_risk_score > 0.0 && c.base_risk_score < 100.0);
    }

    #[test]
    fn test_all_excluded_returns_zero() {
        let dims = [
            dim(10.0, Confidence::NotApplicable),
            dim(20.0, Confidence::NotApplicable),
            dim(30.0, Confidence::NotApplicable),
            dim(40.0, Confidence::NotApplicable),
            dim(50.0, Confidence::NotApplicable),
        ];
        let c = calculate_base_risk_score(&dims);
        assert_eq!(c.base_risk_score, 0.0);
    }

    #[test]
    fn test_final_heat_score() {
        assert_eq!(calculate_final_heat_score(50.0, 1.0), 50.0);
        assert!((calculate_final_heat_score(50.0, 1.05) - 52.5).abs() < 0.01);
        assert_eq!(calculate_final_heat_score(150.0, 1.0), 100.0);
        assert_eq!(calculate_final_heat_score(-10.0, 1.0), 0.0);
    }
}
