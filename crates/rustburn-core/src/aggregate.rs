//! 仓库级聚合（repo_total / top_risk_files）。
//!
//! 与 [crate::scoring] 分开存放，确保 scoring.rs 只消费五个
//! [crate::model::DimensionResult]（SPEC v2 §7 禁止事项 7-A）。

use crate::model::FileScore;

/// 计算仓库总热度分数。
///
/// ```text
/// weighted_mean = Σ(final_heat_score × file_loc_ratio)
/// top_5pct_penalty = top_files_avg × 0.2
/// repo_total_heat_score = weighted_mean + top_5pct_penalty
/// ```
pub fn calculate_repo_total_heat_score(files: &[FileScore]) -> f64 {
    let total_loc: u32 = files.iter().map(|f| f.raw.loc).sum();
    if total_loc == 0 {
        return 0.0;
    }

    let weighted_mean: f64 = files
        .iter()
        .map(|f| f.final_heat_score * f.raw.loc as f64 / total_loc as f64)
        .sum();

    let top_risk_files = calculate_top_risk_files(files);
    let top_5pct_penalty = if top_risk_files.is_empty() {
        0.0
    } else {
        let top_files_avg: f64 = top_risk_files
            .iter()
            .map(|f| f.final_heat_score)
            .sum::<f64>()
            / top_risk_files.len() as f64;
        top_files_avg * 0.2
    };

    (weighted_mean + top_5pct_penalty).clamp(0.0, 100.0)
}

/// 获取风险最高的文件（Top 5%）。
///
/// - 数量：ceil(file_count × 0.05)，最少 1；
/// - 按 final_heat_score 降序，分数相同按路径字典序升序（保证确定性）。
pub fn calculate_top_risk_files(files: &[FileScore]) -> Vec<FileScore> {
    if files.is_empty() {
        return Vec::new();
    }
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| {
        b.final_heat_score
            .partial_cmp(&a.final_heat_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.raw.path.cmp(&b.raw.path))
    });
    let count = (files.len() as f64 * 0.05).ceil() as usize;
    let count = count.max(1).min(files.len());
    sorted.into_iter().take(count).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Confidence, ConsistencyReport, DimensionResult, FileRawMetrics, FileScore,
        HistoryRewriteState, Language, Severity,
    };

    fn score(path: &str, loc: u32, heat: f64) -> FileScore {
        FileScore {
            raw: FileRawMetrics {
                path: path.to_string(),
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
            dimensions: vec![DimensionResult {
                raw_value: 0.0,
                risk_score: 0.0,
                confidence: Confidence::Full,
                detail: serde_json::Value::Null,
            }],
            base_risk_score: heat,
            consistency: ConsistencyReport {
                coverage_report_stale: false,
                history_rewrite: HistoryRewriteState::Unknown,
                lockfile_mismatch: false,
                coefficient: 1.0,
            },
            trend_coefficient: 1.0,
            final_heat_score: heat,
            trend_history: vec![],
        }
    }

    #[test]
    fn test_repo_total_heat_score() {
        let files = vec![score("a.rs", 100, 50.0), score("b.rs", 200, 70.0)];
        let total = calculate_repo_total_heat_score(&files);
        // weighted = 50*1/3 + 70*2/3 = 63.33；top1 avg = 70，penalty = 14 → 77.33
        assert!((total - 77.33).abs() < 0.01, "{}", total);
    }

    #[test]
    fn test_repo_total_empty_or_zero_loc() {
        assert_eq!(calculate_repo_total_heat_score(&[]), 0.0);
        assert_eq!(
            calculate_repo_total_heat_score(&[score("a.rs", 0, 50.0)]),
            0.0
        );
    }

    #[test]
    fn test_top_risk_files_order_and_tiebreak() {
        let files = vec![
            score("b.rs", 100, 50.0),
            score("a.rs", 100, 50.0),
            score("c.rs", 100, 90.0),
        ];
        let top = calculate_top_risk_files(&files);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].raw.path, "c.rs");
        // 同分按路径升序
        let files = vec![score("b.rs", 10, 50.0), score("a.rs", 10, 50.0)];
        let top = calculate_top_risk_files(&files);
        assert_eq!(top[0].raw.path, "a.rs");
    }
}
