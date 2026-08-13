//! 回归测试：v2 五维度评分合成全链路（覆盖原 v0.1.1 修复点）。
//!
//! 验证：
//! - 单一维度极端高分不会被合成公式封顶到 100；
//! - 只有 1 个维度差的文件，分数明显低于五个维度都差的文件；
//! - NotApplicable 维度被排除后权重重新分摊、总和仍为 1.0；
//! - 仓库总热度分数不会因极端数据精确等于 100。

use rustburn_core::model::{Confidence, DimensionResult};
use rustburn_core::scoring::calculate_base_risk_score;
use serde_json::json;

fn dim(risk: f64, confidence: Confidence) -> DimensionResult {
    DimensionResult {
        raw_value: risk,
        risk_score: risk,
        confidence,
        detail: json!({}),
    }
}

fn all_dims(risks: [f64; 5]) -> [DimensionResult; 5] {
    [
        dim(risks[0], Confidence::Full),
        dim(risks[1], Confidence::Full),
        dim(risks[2], Confidence::Full),
        dim(risks[3], Confidence::Full),
        dim(risks[4], Confidence::Full),
    ]
}

/// 回归 1：单一维度为 100 时总分不得封顶到 100。
#[test]
fn single_dimension_100_must_not_cap_total() {
    let dims = all_dims([100.0, 5.0, 5.0, 5.0, 5.0]);
    let c = calculate_base_risk_score(&dims);
    // weighted = 30 + 0.75+1.25+1+0.5 = 33.5；penalty = (100-5)*0.15 = 14.25 → 47.75
    assert!(c.base_risk_score < 100.0, "单一维度 100 不得封顶总分");
}

/// 回归 2：只有 1 个维度差（90+）的文件，分数必须明显低于五维度都差的文件。
#[test]
fn single_bad_dimension_must_score_below_all_bad() {
    let single_bad = all_dims([95.0, 5.0, 5.0, 5.0, 5.0]);
    let all_bad = all_dims([95.0, 95.0, 95.0, 95.0, 95.0]);

    let s_single = calculate_base_risk_score(&single_bad);
    let s_all = calculate_base_risk_score(&all_bad);
    assert!(
        s_all.base_risk_score > s_single.base_risk_score + 10.0,
        "单维差({:.1}) 不应与五维都差({:.1})得分相当",
        s_single.base_risk_score,
        s_all.base_risk_score
    );
}

/// 回归 3：NotApplicable 维度被排除后，权重重新分摊且总和保持 1.0。
#[test]
fn excluded_dimension_renormalizes_weights() {
    let dims = [
        dim(30.0, Confidence::Full),
        dim(40.0, Confidence::NotApplicable),
        dim(50.0, Confidence::Full),
        dim(60.0, Confidence::Full),
        dim(70.0, Confidence::Full),
    ];
    let c = calculate_base_risk_score(&dims);
    assert_eq!(c.excluded_dimensions, vec!["duplication"]);
    let wsum: f64 = c.normalized_weights.iter().map(|(_, w)| *w).sum();
    assert!((wsum - 1.0).abs() < 1e-9, "权重分摊后总和必须为 1.0");
}

/// 回归 4：仓库总热度分数在全链路下不应为精确 100.0。
#[test]
fn repo_total_not_exactly_100() {
    // 5 个中等文件 + 1 个极端文件
    let mut scores = Vec::new();
    for _ in 0..5 {
        scores.push(score_from_dims(
            all_dims([10.0, 10.0, 10.0, 10.0, 10.0]),
            100,
        ));
    }
    scores.push(score_from_dims(
        all_dims([100.0, 95.0, 95.0, 95.0, 95.0]),
        500,
    ));

    let total = rustburn_core::aggregate::calculate_repo_total_heat_score(&scores);
    assert!(
        (total - 100.0).abs() > 1e-6,
        "repo_total 不应为精确 100.0，实际 {:.4}",
        total
    );
    assert!((0.0..=100.0).contains(&total));
}

/// 构造一个最小可用的 FileScore。
fn score_from_dims(dims: [DimensionResult; 5], loc: u32) -> rustburn_core::model::FileScore {
    use rustburn_core::model::{
        ConsistencyReport, FileRawMetrics, FileScore, HistoryRewriteState, Language, Severity,
    };

    let composition = calculate_base_risk_score(&dims);
    FileScore {
        raw: FileRawMetrics {
            path: "test.rs".to_string(),
            language: Language::Rust,
            loc,
            cyclomatic_complexity: 1,
            max_if_nesting_depth: 0,
            nested_if_ratio: 0.0,
            avg_function_length: 0.0,
            max_function_length: 0,
            commit_count: 0,
            distinct_authors: 0,
            last_modified_days_ago: 0,
            incident_commit_count: 0,
            max_cve_severity: Severity::None,
            cve_count: 0,
            dependency_staleness: 0.0,
            dependency_data_incomplete: false,
            parse_incomplete: false,
        },
        dimensions: dims.to_vec(),
        base_risk_score: composition.base_risk_score,
        consistency: ConsistencyReport {
            coverage_report_stale: false,
            history_rewrite: HistoryRewriteState::Unknown,
            lockfile_mismatch: false,
            coefficient: 1.0,
        },
        trend_coefficient: 1.0,
        final_heat_score: composition.base_risk_score,
        trend_history: vec![],
    }
}
