//! 回归测试：新百分位公式与评分全链路（覆盖 v0.1.1 修复点）。

use rustburn_core::model::{DimensionValues, FilePercentileScores};
use rustburn_core::scoring::{
    calculate_base_risk_score, calculate_percentile_scores, calculate_repo_total_heat_score,
};

fn dims(c: f64, h: f64, d: f64) -> DimensionValues {
    DimensionValues {
        complexity_value: c,
        history_value: h,
        dependency_value: d,
    }
}

/// 回归 1：单一维度为 100 分位时总分不得被 cbrt(0) 封顶到 100。
#[test]
fn single_dimension_100_must_not_cap_total() {
    // 3 个文件，其中一个复杂度遥遥领先（百分位必为 100）
    let all = vec![
        dims(5.0, 20.0, 0.0),
        dims(5.0, 20.0, 0.0),
        dims(500.0, 20.0, 0.0),
    ];
    let p = calculate_percentile_scores(&all[2], &all);
    assert_eq!(p.complexity_risk, 100.0, "最大值维度应为 100 分位");

    let base = calculate_base_risk_score(&p);
    assert!(base < 100.0, "单一维度 100 不得封顶总分，实际 {:.2}", base);
}

/// 回归 2：只有 1 个维度差（90+）的文件，分数必须明显低于三维度都差的文件。
#[test]
fn single_bad_dimension_must_score_below_all_bad() {
    let single_bad = FilePercentileScores {
        complexity_risk: 95.0,
        history_risk: 5.0,
        dependency_risk: 5.0,
    };
    let all_bad = FilePercentileScores {
        complexity_risk: 95.0,
        history_risk: 95.0,
        dependency_risk: 95.0,
    };
    let s_single = calculate_base_risk_score(&single_bad);
    let s_all = calculate_base_risk_score(&all_bad);
    assert!(
        s_all > s_single + 10.0,
        "单维差({:.1}) 不应与三维都差({:.1})得分相当",
        s_single,
        s_all
    );
}

/// 回归 3：9 个依赖风险为 0 的文件，依赖百分位必须显著低于 1 个依赖风险 80 的文件。
#[test]
fn zero_dependency_risk_ranks_low_in_pipeline() {
    let zero = dims(10.0, 20.0, 0.0);
    let high = dims(10.0, 20.0, 80.0);
    let mut all = vec![zero.clone(); 9];
    all.push(high.clone());

    for i in 0..all.len() {
        let p = calculate_percentile_scores(&all[i], &all);
        if i < 9 {
            assert!(
                p.dependency_risk < 50.0,
                "依赖风险 0 应处低百分位，实际 {:.1}",
                p.dependency_risk
            );
        } else {
            assert!(
                p.dependency_risk >= 90.0,
                "依赖风险 80 应处高百分位，实际 {:.1}",
                p.dependency_risk
            );
        }
    }
}

/// 回归 4：仓库总热度分数在全链路下不应为精确 100.0（旧公式对极端数据的封顶）。
#[test]
fn repo_total_not_exactly_100() {
    let zero = dims(10.0, 20.0, 0.0);
    let high = dims(500.0, 95.0, 80.0);
    let mut all = vec![zero.clone(); 9];
    all.push(high);

    let mut scores = Vec::new();
    for i in 0..all.len() {
        let p = calculate_percentile_scores(&all[i], &all);
        let base = calculate_base_risk_score(&p);
        // 用 FileScore 的最小字段构造（final = base * 1.0）
        scores.push(score_from_base(&all[i], p, base));
    }

    let total = calculate_repo_total_heat_score(&scores);
    assert!(
        (total - 100.0).abs() > 1e-6,
        "repo_total 不应为精确 100.0，实际 {:.4}",
        total
    );
    assert!((0.0..=100.0).contains(&total));
}

/// 构造一个最小可用的 FileScore（趋势系数恒 1.0）。
fn score_from_base(
    dim: &DimensionValues,
    p: FilePercentileScores,
    base: f64,
) -> rustburn_core::model::FileScore {
    use rustburn_core::model::{
        ConsistencyReport, FileRawMetrics, FileScore, HistoryRewriteState, Language, Severity,
    };

    FileScore {
        raw: FileRawMetrics {
            path: "test.rs".to_string(),
            language: Language::Rust,
            loc: 10,
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
        percentiles: p,
        dimension_values: dim.clone(),
        base_risk_score: base,
        consistency: ConsistencyReport {
            coverage_report_stale: false,
            history_rewrite: HistoryRewriteState::Unknown,
            lockfile_mismatch: false,
            coefficient: 1.0,
        },
        trend_coefficient: 1.0,
        final_heat_score: base,
        trend_history: vec![],
    }
}
