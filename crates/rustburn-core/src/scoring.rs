//! 评分算法模块。
//!
//! 实现维度综合值、percentile、base_risk_score、consistency_coefficient、
//! trend_coefficient、final_heat_score、repo_total_heat_score 计算。
//! 所有公式严格遵循 spec.md 定义。

use crate::model::{
    DimensionValues, FilePercentileScores, FileRawMetrics, FileScore, HistoricalSnapshot,
    HistoryRewriteState,
};

/// 计算维度综合值。
///
/// 根据 spec §68：
/// - complexity_value = cyclomatic_complexity * 0.4 + max_if_nesting_depth * 15.0 * 0.4 + avg_function_length * 0.2
/// - history_value = normalized_commit_count * 0.35 + normalized_distinct_authors * 0.10 + normalized_incident_commit_count * 0.45 + normalized_recency * 0.10
/// - dependency_value = severity_score * 0.60 + normalized_cve_count * 0.25 + dependency_staleness * 0.15
pub fn calculate_dimension_values(
    metrics: &FileRawMetrics,
    max_commit_count: u32,
    max_author_count: u32,
    max_incident_count: u32,
    max_cve_count: u32,
) -> DimensionValues {
    let values = DimensionValues {
        complexity_value: calculate_complexity_value(metrics),
        history_value: calculate_history_value(
            metrics,
            max_commit_count,
            max_author_count,
            max_incident_count,
        ),
        dependency_value: calculate_dependency_value(metrics, max_cve_count),
    };

    if crate::debug_enabled() {
        crate::debug_log(format_args!(
            "dimension_values path={} C={:.3} H={:.3} D={:.3} (raw: cc={} depth={} avg_len={:.1} commits={} authors={} incidents={} recency_days={} severity={} cves={})",
            metrics.path,
            values.complexity_value,
            values.history_value,
            values.dependency_value,
            metrics.cyclomatic_complexity,
            metrics.max_if_nesting_depth,
            metrics.avg_function_length,
            metrics.commit_count,
            metrics.distinct_authors,
            metrics.incident_commit_count,
            metrics.last_modified_days_ago,
            metrics.max_cve_severity,
            metrics.cve_count,
        ));
    }

    values
}

/// 复杂度维度综合值（spec §68）。
fn calculate_complexity_value(metrics: &FileRawMetrics) -> f64 {
    let value = metrics.cyclomatic_complexity as f64 * 0.4
        + metrics.max_if_nesting_depth as f64 * 15.0 * 0.4
        + metrics.avg_function_length * 0.2;
    value.clamp(0.0, 100.0)
}

/// 历史维度综合值（spec §68，各指标归一化到 0-100）。
fn calculate_history_value(
    metrics: &FileRawMetrics,
    max_commit_count: u32,
    max_author_count: u32,
    max_incident_count: u32,
) -> f64 {
    let normalized_commit_count = normalize_value(metrics.commit_count, max_commit_count);
    let normalized_authors = normalize_value(metrics.distinct_authors, max_author_count);
    let normalized_incidents = normalize_value(metrics.incident_commit_count, max_incident_count);
    let normalized_recency = calculate_recency_risk(metrics.last_modified_days_ago);

    let value = normalized_commit_count * 0.35
        + normalized_authors * 0.10
        + normalized_incidents * 0.45
        + normalized_recency * 0.10;
    value.clamp(0.0, 100.0)
}

/// 依赖维度综合值（spec §68）。
fn calculate_dependency_value(metrics: &FileRawMetrics, max_cve_count: u32) -> f64 {
    let severity_score = metrics.max_cve_severity.to_score();
    let normalized_cve_count = normalize_value(metrics.cve_count, max_cve_count);
    let staleness_score = metrics.dependency_staleness * 100.0;

    let value = severity_score * 0.60 + normalized_cve_count * 0.25 + staleness_score * 0.15;
    value.clamp(0.0, 100.0)
}

/// 将值归一化到 0-100 范围。
fn normalize_value(value: u32, max_value: u32) -> f64 {
    if max_value == 0 {
        return 0.0;
    }
    ((value as f64 / max_value as f64) * 100.0).clamp(0.0, 100.0)
}

/// 计算新鲜度风险（越新风险越低）。
///
/// 根据 last_modified_days_ago：
/// - 0-30 天：0（低风险）
/// - 31-90 天：33
/// - 91-180 天：66
/// - >180 天：100（高风险）
fn calculate_recency_risk(last_modified_days_ago: u32) -> f64 {
    match last_modified_days_ago {
        0..=30 => 0.0,
        31..=90 => 33.0,
        91..=180 => 66.0,
        _ => 100.0,
    }
}

/// 计算 percentile 分数。
///
/// 根据 spec §70-71：
/// - 按升序排序
/// - 相同值获得相同 rank（取最后位置）
/// - percentile = r / file_count * 100，r 从 1 开始
/// - 单文件仓库：percentile = 50 并产生 warning
pub fn calculate_percentile_scores(
    current: &DimensionValues,
    all_dimension_values: &[DimensionValues],
) -> FilePercentileScores {
    // 无数据或单文件仓库：无法计算相对排名，统一返回 50
    if all_dimension_values.len() <= 1 {
        return FilePercentileScores {
            complexity_risk: 50.0,
            history_risk: 50.0,
            dependency_risk: 50.0,
        };
    }

    // 提取各维度的值
    let complexity_values: Vec<f64> = all_dimension_values
        .iter()
        .map(|d| d.complexity_value)
        .collect();
    let history_values: Vec<f64> = all_dimension_values
        .iter()
        .map(|d| d.history_value)
        .collect();
    let dependency_values: Vec<f64> = all_dimension_values
        .iter()
        .map(|d| d.dependency_value)
        .collect();

    // 计算当前文件的维度值
    let current_complexity = current.complexity_value;
    let current_history = current.history_value;
    let current_dependency = current.dependency_value;

    let scores = FilePercentileScores {
        complexity_risk: calculate_percentile(current_complexity, &complexity_values),
        history_risk: calculate_percentile(current_history, &history_values),
        dependency_risk: calculate_percentile(current_dependency, &dependency_values),
    };

    if crate::debug_enabled() {
        crate::debug_log(format_args!(
            "percentile_scores n={} current=(C={:.3} H={:.3} D={:.3}) -> pC={:.2} pH={:.2} pD={:.2} (C pool {} | H pool {} | D pool {})",
            all_dimension_values.len(),
            current_complexity,
            current_history,
            current_dependency,
            scores.complexity_risk,
            scores.history_risk,
            scores.dependency_risk,
            format_f64s(&complexity_values),
            format_f64s(&history_values),
            format_f64s(&dependency_values),
        ));
    }

    scores
}

/// 将 f64 集合格式化为紧凑字符串（供 RB_DEBUG 日志使用）。
fn format_f64s(values: &[f64]) -> String {
    values
        .iter()
        .map(|v| format!("{:.2}", v))
        .collect::<Vec<_>>()
        .join(",")
}

/// 仓库内百分位在最终风险中的权重（初始各占 0.5，后续用真实项目数据校准）。
pub const W_PERCENTILE: f64 = 0.5;
/// 绝对阈值映射在最终风险中的权重。
pub const W_ABSOLUTE: f64 = 0.5;

/// 每个维度的最终风险值 = w1 * 仓库内百分位 + w2 * 绝对阈值映射分数。
///
/// 百分位反映"相对排名"，绝对阈值反映"行业公认的客观风险"，
/// 二者混合避免小样本仓库靠内部相对排名人为制造高分文件。
pub fn blend_percentile_and_absolute(
    percentiles: &FilePercentileScores,
    absolute: &FilePercentileScores,
) -> FilePercentileScores {
    FilePercentileScores {
        complexity_risk: W_PERCENTILE * percentiles.complexity_risk
            + W_ABSOLUTE * absolute.complexity_risk,
        history_risk: W_PERCENTILE * percentiles.history_risk + W_ABSOLUTE * absolute.history_risk,
        dependency_risk: W_PERCENTILE * percentiles.dependency_risk
            + W_ABSOLUTE * absolute.dependency_risk,
    }
}

/// 计算文件在三个维度上的绝对阈值映射分数（0-100，越高风险越大）。
///
/// 标准来源（均采用行业公认经验值，非 rustburn 自创）：
/// - 复杂度：McCabe 圈复杂度阈值（<10 低 / 10-20 中 / 20-50 高 / >50 严重），
///   以及 ESLint `max-depth` 规则默认上限（4 层）；
/// - 历史：业界通用的代码陈旧周期（30 / 90 / 180 / 365 天）；
/// - 依赖：CVSS 严重度官方分档（None/Low/Medium/High/Critical），
///   多漏洞数量档位为经验值，待真实项目数据校准。
pub fn absolute_risk_scores(metrics: &FileRawMetrics) -> FilePercentileScores {
    FilePercentileScores {
        complexity_risk: absolute_complexity(metrics),
        history_risk: absolute_history(metrics),
        dependency_risk: absolute_dependency(metrics),
    }
}

/// 复杂度绝对分数：圈复杂度（McCabe）0.7 + 嵌套深度（ESLint max-depth）0.3。
fn absolute_complexity(metrics: &FileRawMetrics) -> f64 {
    0.7 * cc_band(metrics.cyclomatic_complexity) + 0.3 * depth_band(metrics.max_if_nesting_depth)
}

/// McCabe 圈复杂度阈值映射。
fn cc_band(cc: u32) -> f64 {
    match cc {
        0..=9 => 15.0,   // 低
        10..=19 => 50.0, // 中
        20..=49 => 80.0, // 高
        _ => 100.0,      // 严重
    }
}

/// if 嵌套深度阈值映射（ESLint max-depth 默认上限 4 层）。
fn depth_band(depth: u32) -> f64 {
    match depth {
        0..=4 => 15.0,  // 低
        5..=7 => 50.0,  // 中
        8..=10 => 80.0, // 高
        _ => 100.0,     // 严重
    }
}

/// 历史绝对分数：以陈旧度为准（30/90/180/365 天陈旧周期）。
fn absolute_history(metrics: &FileRawMetrics) -> f64 {
    calculate_recency_risk(metrics.last_modified_days_ago)
}

/// 依赖绝对分数：CVSS 严重度 0.6 + CVE 数量 0.25（过时程度暂恒为 0）。
fn absolute_dependency(metrics: &FileRawMetrics) -> f64 {
    let severity_score = metrics.max_cve_severity.to_score();
    let cve_band = match metrics.cve_count {
        0 => 0.0,
        1 => 60.0,
        2..=4 => 80.0,
        _ => 100.0,
    };
    (severity_score * 0.6 + cve_band * 0.25).clamp(0.0, 100.0)
}

/// 计算单个值的 percentile。
///
/// - 升序排序；
/// - 相同值获得相同 rank，取**最小** rank（第一个出现的位置）。
///   这样大量相同低值（例如依赖风险普遍为 0）不会被互相抬高，
///   低风险值落在低百分位、最大值始终落在 100 分位；
/// - percentile = r / file_count * 100，r 从 1 开始。
fn calculate_percentile(value: f64, all_values: &[f64]) -> f64 {
    if all_values.is_empty() {
        return 50.0;
    }

    // 所有值相同：该维度无法区分文件，返回中性 50
    // （避免无区分度的维度被顶到 100 分位后拉满 base_risk）
    if all_values.iter().all(|v| *v == all_values[0]) {
        return 50.0;
    }

    let mut sorted = all_values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // 找到 value 在排序数组中的最小 rank（第一个 >= value 的位置，r 从 1 开始）
    let mut r = 0;
    for (i, &v) in sorted.iter().enumerate() {
        if v >= value {
            r = i + 1;
            break;
        }
    }

    if r == 0 {
        // value 大于所有值
        r = sorted.len();
    }

    let percentile = (r as f64 / sorted.len() as f64) * 100.0;
    percentile.clamp(0.0, 100.0)
}

/// 计算基础风险分数。
///
/// 采用「加权算术平均 + 高风险维度惩罚」：
///
/// ```text
/// base_risk = w1*c + w2*h + w3*d + extra_penalty(max(c,h,d))
/// ```
///
/// - 权重：复杂度 60%、历史 30%、依赖 10%（与报告 UI 展示一致）；
/// - `extra_penalty` 仅在某一维度**显著超过**其余两维度均值时追加
///   （max > 50 且 max > mean_of_others * 1.25），幅度为
///   `(max - mean_of_others) * 0.15`；
/// - 因此单一维度为 100 分位时，总分**不会被封顶到 100**，
///   其余两个维度的实际值仍然有效影响结果。
pub fn calculate_base_risk_score(percentiles: &FilePercentileScores) -> f64 {
    const W_COMPLEXITY: f64 = 0.6;
    const W_HISTORY: f64 = 0.3;
    const W_DEPENDENCY: f64 = 0.1;

    let c = percentiles.complexity_risk;
    let h = percentiles.history_risk;
    let d = percentiles.dependency_risk;

    let base = W_COMPLEXITY * c + W_HISTORY * h + W_DEPENDENCY * d;
    let extra = extra_penalty(c, h, d);
    let result = (base + extra).clamp(0.0, 100.0);

    if crate::debug_enabled() {
        crate::debug_log(format_args!(
            "base_risk pC={:.2} pH={:.2} pD={:.2} weighted={:.3} extra_penalty={:.3} -> base_risk={:.3}",
            c, h, d, base, extra, result
        ));
    }

    result
}

/// 高风险维度惩罚。
///
/// 仅当最大值显著偏离其余两个维度均值时才追加惩罚，避免
/// 单一维度 100 分直接决定总分、也避免平庸文件被无意义放大。
fn extra_penalty(c: f64, h: f64, d: f64) -> f64 {
    let dims = [c, h, d];
    let max_dim = dims.iter().copied().fold(f64::MIN, f64::max);
    let mean_of_others = (c + h + d - max_dim) / 2.0;

    // 显著偏离阈值：最大维度超过其余均值 25% 以上，且本身超过中性值 50
    if max_dim > 50.0 && max_dim > mean_of_others * 1.25 {
        (max_dim - mean_of_others) * 0.15
    } else {
        0.0
    }
}

/// 计算一致性系数。
///
/// 根据 spec §74：
/// - 初始：1.0
/// - 如果 history_rewrite = detected，乘 0.7
/// - 如果 coverage_report_stale = true，乘 0.85
/// - 如果 lockfile_mismatch = true，乘 0.9
/// - 最终：max(coefficient, 0.5)
pub fn calculate_consistency_coefficient(
    coverage_report_stale: bool,
    history_rewrite: HistoryRewriteState,
    lockfile_mismatch: bool,
) -> f64 {
    let mut coefficient: f64 = 1.0;

    // 根据 spec §75，Unknown 状态不影响系数
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
///
/// 根据 spec §81：
/// - historical_mean = 所有有效历史 snapshot base_risk_score 的算术平均
/// - current = 最新有效 snapshot base_risk_score
/// - trend_delta = (historical_mean - current) / max(historical_mean, 1.0)
/// - trend_delta ∈ [-0.3, 0.3]
/// - trend_coefficient = 1 - trend_delta * 0.3
/// - 理论范围：[0.91, 1.09]
///
/// 如果没有足够有效历史 snapshot，返回 1.0
pub fn calculate_trend_coefficient(snapshots: &[HistoricalSnapshot]) -> f64 {
    if snapshots.is_empty() {
        if crate::debug_enabled() {
            crate::debug_log(format_args!(
                "trend_coefficient snapshots=0 -> 1.0 (trend analysis not enabled)"
            ));
        }
        return 1.0;
    }

    // 计算历史平均
    let historical_mean: f64 =
        snapshots.iter().map(|s| s.base_risk_score).sum::<f64>() / snapshots.len() as f64;

    // 获取当前（最新）值
    let current = snapshots.last().map(|s| s.base_risk_score).unwrap_or(0.0);

    // 计算 trend_delta
    let trend_delta = (historical_mean - current) / historical_mean.max(1.0);

    // 限制范围
    let trend_delta = trend_delta.clamp(-0.3, 0.3);

    // 计算趋势系数
    let trend_coefficient = 1.0 - trend_delta * 0.3;

    // 限制在理论范围内
    let result = trend_coefficient.clamp(0.91, 1.09);

    if crate::debug_enabled() {
        crate::debug_log(format_args!(
            "trend_coefficient snapshots={} historical_mean={:.3} current={:.3} delta={:.3} -> {:.3}",
            snapshots.len(),
            historical_mean,
            current,
            trend_delta,
            result
        ));
    }

    result
}

/// 计算最终热度分数。
///
/// 根据 spec §84：
/// final_heat_score = base_risk_score * trend_coefficient
/// clamp(final_heat_score, 0, 100)
///
/// 注意：consistency_coefficient 不得参与 final_heat_score 计算（spec §76）
pub fn calculate_final_heat_score(base_risk_score: f64, trend_coefficient: f64) -> f64 {
    let score = base_risk_score * trend_coefficient;
    score.clamp(0.0, 100.0)
}

/// 计算仓库总热度分数。
///
/// 根据 spec §85-88：
/// - weighted_mean = sum(final_heat_score * file_loc_ratio)
/// - file_loc_ratio = file_loc / total_repo_loc
/// - top_files_avg = top_files.final_heat_score 的算术平均
/// - top_5pct_penalty = top_files_avg * 0.2
/// - repo_total_heat_score = weighted_mean + top_5pct_penalty
/// - clamp(repo_total_heat_score, 0, 100)
pub fn calculate_repo_total_heat_score(files: &[FileScore]) -> f64 {
    let total_loc: u32 = files.iter().map(|f| f.raw.loc).sum();

    // 无文件或无有效 LOC 时直接返回 0
    if total_loc == 0 {
        return 0.0;
    }

    let weighted_mean = calculate_loc_weighted_mean(files, total_loc);
    let top_5pct_penalty = calculate_top_5pct_penalty(files);

    (weighted_mean + top_5pct_penalty).clamp(0.0, 100.0)
}

/// 按 LOC 加权平均 final_heat_score（spec §85）。
fn calculate_loc_weighted_mean(files: &[FileScore], total_loc: u32) -> f64 {
    files
        .iter()
        .map(|f| f.final_heat_score * f.raw.loc as f64 / total_loc as f64)
        .sum()
}

/// Top 5% 文件平均分 * 0.2 惩罚（spec §87）。
fn calculate_top_5pct_penalty(files: &[FileScore]) -> f64 {
    let top_risk_files = calculate_top_risk_files(files);
    if top_risk_files.is_empty() {
        return 0.0;
    }

    let top_files_avg: f64 = top_risk_files
        .iter()
        .map(|f| f.final_heat_score)
        .sum::<f64>()
        / top_risk_files.len() as f64;
    top_files_avg * 0.2
}

/// 获取风险最高的文件（Top 5%）。
///
/// 根据 spec §87-89：
/// - 数量：ceil(file_count * 0.05)，最少 1
/// - 按 final_heat_score 降序
/// - 分数相同则按路径字典序升序
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
    use crate::model::{ConsistencyReport, Language, Severity};

    fn create_test_metrics() -> FileRawMetrics {
        FileRawMetrics {
            path: "test.rs".to_string(),
            language: Language::Rust,
            loc: 100,
            cyclomatic_complexity: 10,
            max_if_nesting_depth: 2,
            nested_if_ratio: 0.3,
            avg_function_length: 20.0,
            max_function_length: 50,
            commit_count: 5,
            distinct_authors: 2,
            last_modified_days_ago: 30,
            incident_commit_count: 1,
            max_cve_severity: Severity::None,
            cve_count: 0,
            dependency_staleness: 0.0,
            dependency_data_incomplete: false,
            parse_incomplete: false,
        }
    }

    #[test]
    fn test_dimension_values() {
        let metrics = create_test_metrics();
        let dims = calculate_dimension_values(&metrics, 10, 5, 3, 5);

        // complexity_value = 10 * 0.4 + 2 * 15.0 * 0.4 + 20.0 * 0.2 = 4 + 12 + 4 = 20
        assert!((dims.complexity_value - 20.0).abs() < 0.01);

        // history_value 需要归一化
        assert!(dims.history_value >= 0.0 && dims.history_value <= 100.0);

        // dependency_value = 0 * 0.6 + 0 * 0.25 + 0 * 0.15 = 0
        assert!((dims.dependency_value - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_percentile_single_file() {
        let metrics = create_test_metrics();
        let dims = vec![calculate_dimension_values(&metrics, 10, 5, 3, 5)];
        let percentiles = calculate_percentile_scores(&dims[0], &dims);

        // 单文件仓库应返回 50
        assert_eq!(percentiles.complexity_risk, 50.0);
        assert_eq!(percentiles.history_risk, 50.0);
        assert_eq!(percentiles.dependency_risk, 50.0);
    }

    #[test]
    fn test_absolute_risk_complexity_bands() {
        // McCabe 圈复杂度阈值：<10 低 / 10-20 中 / 20-50 高 / >50 严重
        let mut m = create_test_metrics();
        m.cyclomatic_complexity = 7;
        m.max_if_nesting_depth = 2;
        let abs = absolute_risk_scores(&m);
        // 0.7*15 + 0.3*15 = 15
        assert!(
            (abs.complexity_risk - 15.0).abs() < 1e-9,
            "got {}",
            abs.complexity_risk
        );

        m.cyclomatic_complexity = 25;
        m.max_if_nesting_depth = 9;
        let abs = absolute_risk_scores(&m);
        // 0.7*80 + 0.3*80 = 80
        assert!(
            (abs.complexity_risk - 80.0).abs() < 1e-9,
            "got {}",
            abs.complexity_risk
        );

        // 圈复杂度中等、深度低：加权混合
        m.cyclomatic_complexity = 15; // 中=50
        m.max_if_nesting_depth = 2; // 低=15
        let abs = absolute_risk_scores(&m);
        // 0.7*50 + 0.3*15 = 39.5
        assert!(
            (abs.complexity_risk - 39.5).abs() < 1e-9,
            "got {}",
            abs.complexity_risk
        );
    }

    #[test]
    fn test_absolute_risk_history_recency() {
        let mut m = create_test_metrics();
        m.last_modified_days_ago = 10; // 新鲜
        assert_eq!(absolute_risk_scores(&m).history_risk, 0.0);

        m.last_modified_days_ago = 200; // 陈旧 >180 天
        assert_eq!(absolute_risk_scores(&m).history_risk, 100.0);
    }

    #[test]
    fn test_absolute_risk_dependency_cvss_and_count() {
        // 无漏洞 → 0
        let m = create_test_metrics();
        assert_eq!(absolute_risk_scores(&m).dependency_risk, 0.0);

        // Medium + 3 个 CVE → 0.6*50 + 0.25*80 = 50
        let mut m = create_test_metrics();
        m.max_cve_severity = Severity::Medium;
        m.cve_count = 3;
        assert!(
            (absolute_risk_scores(&m).dependency_risk - 50.0).abs() < 1e-9,
            "got {}",
            absolute_risk_scores(&m).dependency_risk
        );
    }

    #[test]
    fn test_blend_weights_half_and_half() {
        // 百分位 100 vs 绝对 0 → 混合后 50（不再被单一来源封顶）
        let pct = FilePercentileScores {
            complexity_risk: 100.0,
            history_risk: 0.0,
            dependency_risk: 0.0,
        };
        let abs = FilePercentileScores {
            complexity_risk: 0.0,
            history_risk: 0.0,
            dependency_risk: 0.0,
        };
        let blended = blend_percentile_and_absolute(&pct, &abs);
        assert!((blended.complexity_risk - 50.0).abs() < 1e-9);
        assert_eq!(blended.history_risk, 0.0);
        assert_eq!(blended.dependency_risk, 0.0);
    }

    #[test]
    fn test_blend_low_absolute_keeps_healthy_file_low() {
        // 回归验收：圈复杂度 7、嵌套 2 的"客观不差"文件，
        // 即使百分位被推到 100，最终风险也不应虚高到 90+。
        let mut m = create_test_metrics();
        m.cyclomatic_complexity = 7;
        m.max_if_nesting_depth = 2;

        let abs = absolute_risk_scores(&m); // complexity_abs = 15
        let pct = FilePercentileScores {
            complexity_risk: 100.0,
            history_risk: 100.0,
            dependency_risk: 0.0,
        };
        let blended = blend_percentile_and_absolute(&pct, &abs);
        // complexity = 0.5*100 + 0.5*15 = 57.5
        assert!(
            blended.complexity_risk < 60.0,
            "低复杂度文件不应被打成 90+，实际 {}",
            blended.complexity_risk
        );
    }

    #[test]
    fn test_percentile_multiple_files() {
        let mut metrics1 = create_test_metrics();
        metrics1.cyclomatic_complexity = 5;

        let mut metrics2 = create_test_metrics();
        metrics2.cyclomatic_complexity = 10;

        let mut metrics3 = create_test_metrics();
        metrics3.cyclomatic_complexity = 15;

        let dims = vec![
            calculate_dimension_values(&metrics1, 20, 5, 3, 5),
            calculate_dimension_values(&metrics2, 20, 5, 3, 5),
            calculate_dimension_values(&metrics3, 20, 5, 3, 5),
        ];

        // 复杂度值 18 < 20 < 22，percentile 必须递增且能区分文件
        let p1 = calculate_percentile_scores(&dims[0], &dims);
        let p2 = calculate_percentile_scores(&dims[1], &dims);
        let p3 = calculate_percentile_scores(&dims[2], &dims);
        assert!(
            p1.complexity_risk < p2.complexity_risk && p2.complexity_risk < p3.complexity_risk,
            "percentile 应随维度值单调递增: {} / {} / {}",
            p1.complexity_risk,
            p2.complexity_risk,
            p3.complexity_risk
        );

        // metrics2 复杂度处于中位：排序 [18,20,22]，rank=2 → 2/3*100 ≈ 66.7
        assert!((p2.complexity_risk - 200.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn test_percentile_dependency_zero_ranks_low() {
        // 回归验收：构造 10 个文件，9 个依赖风险为 0、1 个为 80。
        // 依赖风险 0 必须落在低百分位，且显著低于依赖风险 80 的文件。
        // （旧实现“同值取最后位置”会让 9 个 0 值互相抬高到 90 分位。）
        let zero = DimensionValues {
            complexity_value: 10.0,
            history_value: 10.0,
            dependency_value: 0.0,
        };
        let high = DimensionValues {
            complexity_value: 10.0,
            history_value: 10.0,
            dependency_value: 80.0,
        };

        let mut all = vec![zero.clone(); 9];
        all.push(high.clone());

        let zero_p = calculate_percentile_scores(&zero, &all);
        let high_p = calculate_percentile_scores(&high, &all);

        assert!(
            zero_p.dependency_risk < 50.0,
            "依赖风险 0 应处于低百分位，实际 {}",
            zero_p.dependency_risk
        );
        assert!(high_p.dependency_risk >= 90.0);
        assert!(
            zero_p.dependency_risk < high_p.dependency_risk,
            "依赖风险 0 的百分位应显著低于依赖风险 80 的文件"
        );
    }

    #[test]
    fn test_percentile_all_values_equal_returns_neutral() {
        // 多文件但某维度所有值相同：该维度无法区分文件，返回中性 50
        let v = DimensionValues {
            complexity_value: 20.0,
            history_value: 30.0,
            dependency_value: 0.0,
        };
        let all = vec![v.clone(), v.clone(), v.clone()];
        let p = calculate_percentile_scores(&v, &all);
        assert_eq!(p.complexity_risk, 50.0);
        assert_eq!(p.history_risk, 50.0);
        assert_eq!(p.dependency_risk, 50.0);
    }

    #[test]
    fn test_percentile_empty_and_extreme_positions() {
        // 空集合 → 中性 50
        assert_eq!(calculate_percentile(10.0, &[]), 50.0);

        let vals = vec![10.0, 20.0, 30.0, 40.0];
        // 低于所有值 → 最小 rank（1/4）
        assert!((calculate_percentile(5.0, &vals) - 25.0).abs() < 1e-9);
        // 高于所有值 → 100 分位
        assert!((calculate_percentile(99.0, &vals) - 100.0).abs() < 1e-9);
        // 恰为最小值 → 1/4
        assert!((calculate_percentile(10.0, &vals) - 25.0).abs() < 1e-9);
        // 恰为最大值 → 100 分位
        assert!((calculate_percentile(40.0, &vals) - 100.0).abs() < 1e-9);
        // 介于中间 → 严格递增
        assert!((calculate_percentile(25.0, &vals) - 75.0).abs() < 1e-9);
    }

    #[test]
    fn test_percentile_ties_share_min_rank() {
        // 同值取最小 rank：低值平局不被抬高、高值平局共享较低分位
        let vals = vec![0.0, 0.0, 80.0, 80.0];
        // 0：第一个 >=0 的位置 0 → rank 1 → 25%
        assert!((calculate_percentile(0.0, &vals) - 25.0).abs() < 1e-9);
        // 80：第一个 >=80 的位置 2 → rank 3 → 75%（不独占 100）
        assert!((calculate_percentile(80.0, &vals) - 75.0).abs() < 1e-9);
    }

    #[test]
    fn test_base_risk_boundary_and_clamp() {
        // max == 50 未超过阈值 → 不触发惩罚：weighted = 30+12+3 = 45
        let b = calculate_base_risk_score(&FilePercentileScores {
            complexity_risk: 50.0,
            history_risk: 40.0,
            dependency_risk: 30.0,
        });
        assert!((b - 45.0).abs() < 1e-9);

        // 三个维度均为 0 → 0；均为 100 → 100
        let zero = calculate_base_risk_score(&FilePercentileScores {
            complexity_risk: 0.0,
            history_risk: 0.0,
            dependency_risk: 0.0,
        });
        let full = calculate_base_risk_score(&FilePercentileScores {
            complexity_risk: 100.0,
            history_risk: 100.0,
            dependency_risk: 100.0,
        });
        assert_eq!(zero, 0.0);
        assert_eq!(full, 100.0);

        // (100,100,50)：weighted=95，max=100 > mean_of_others(75)*1.25=93.75
        // → 惩罚 (100-75)*0.15=3.75 → 98.75，且必须 < 100
        let mix = calculate_base_risk_score(&FilePercentileScores {
            complexity_risk: 100.0,
            history_risk: 100.0,
            dependency_risk: 50.0,
        });
        assert!((mix - 98.75).abs() < 1e-9, "mix={}", mix);
        assert!(mix < 100.0);

        // 负值 / 超大值 clamp 到 [0, 100]
        let neg = calculate_base_risk_score(&FilePercentileScores {
            complexity_risk: -10.0,
            history_risk: -10.0,
            dependency_risk: -10.0,
        });
        let over = calculate_base_risk_score(&FilePercentileScores {
            complexity_risk: 200.0,
            history_risk: 200.0,
            dependency_risk: 200.0,
        });
        assert_eq!(neg, 0.0);
        assert_eq!(over, 100.0);
    }

    #[test]
    fn test_base_risk_score() {
        let percentiles = FilePercentileScores {
            complexity_risk: 50.0,
            history_risk: 50.0,
            dependency_risk: 50.0,
        };

        let base_risk = calculate_base_risk_score(&percentiles);

        // base_risk = 0.6*50 + 0.3*50 + 0.1*50 = 50（无惩罚，max 未超过 50）
        assert!((base_risk - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_base_risk_single_dimension_100_not_capped() {
        // 旧公式缺陷：任一维度为 100 时总分被 cbrt(0) 封顶到 100。
        // 新公式：单一 100 只按权重贡献 + 少量惩罚，总分必须 < 100。
        let percentiles = FilePercentileScores {
            complexity_risk: 100.0,
            history_risk: 0.0,
            dependency_risk: 0.0,
        };

        let base_risk = calculate_base_risk_score(&percentiles);

        // base = 0.6*100 = 60
        // extra = (100 - 0) * 0.15 = 15（max=100 > 50 且 > 0*1.25）
        // total = 75
        assert!(
            (base_risk - 75.0).abs() < 0.01,
            "单一维度 100 不应封顶总分，实际 {}",
            base_risk
        );
        assert!(base_risk < 100.0);
    }

    #[test]
    fn test_base_risk_single_bad_dimension_vs_all_bad() {
        // 回归验收：只有 1 个维度差（90+）的文件，
        // 最终分数不应等同于三个维度都很差的文件。
        let single_bad = FilePercentileScores {
            complexity_risk: 5.0,
            history_risk: 5.0,
            dependency_risk: 95.0,
        };
        let all_bad = FilePercentileScores {
            complexity_risk: 95.0,
            history_risk: 95.0,
            dependency_risk: 95.0,
        };

        let score_single = calculate_base_risk_score(&single_bad);
        let score_all = calculate_base_risk_score(&all_bad);

        // single_bad: base = 0.6*5+0.3*5+0.1*95 = 14；extra = (95-5)*0.15 = 13.5 → 27.5
        // all_bad:    base = 95；extra = 0（max 未超过其余均值 1.25 倍）→ 95
        assert!(
            score_all > score_single + 10.0,
            "单一维度差(={:.1})不应与三维度都差(={:.1})得分相当",
            score_single,
            score_all
        );
    }

    #[test]
    fn test_base_risk_penalty_only_on_significant_imbalance() {
        // 三个维度接近均衡时不应追加惩罚
        let balanced = FilePercentileScores {
            complexity_risk: 80.0,
            history_risk: 70.0,
            dependency_risk: 60.0,
        };
        let score = calculate_base_risk_score(&balanced);
        // base = 48+21+6 = 75；max=80，mean_of_others=65，80 > 65*1.25=81.25 不成立 → 无惩罚
        assert!((score - 75.0).abs() < 0.01);
    }

    #[test]
    fn test_consistency_coefficient() {
        // 无问题
        assert_eq!(
            calculate_consistency_coefficient(false, HistoryRewriteState::NotDetected, false),
            1.0
        );

        // 历史重写检测
        assert_eq!(
            calculate_consistency_coefficient(false, HistoryRewriteState::Detected, false),
            0.7
        );

        // 覆盖率报告过期
        assert_eq!(
            calculate_consistency_coefficient(true, HistoryRewriteState::NotDetected, false),
            0.85
        );

        // lockfile 不匹配
        assert_eq!(
            calculate_consistency_coefficient(false, HistoryRewriteState::NotDetected, true),
            0.9
        );

        // 多个问题
        let coeff = calculate_consistency_coefficient(true, HistoryRewriteState::Detected, true);
        assert!((coeff - 0.7 * 0.85 * 0.9).abs() < 0.01);

        // 下限保护
        assert!(coeff >= 0.5);

        // Unknown 状态不影响系数
        assert_eq!(
            calculate_consistency_coefficient(false, HistoryRewriteState::Unknown, false),
            1.0
        );
    }

    #[test]
    fn test_trend_coefficient() {
        // 无历史数据
        assert_eq!(calculate_trend_coefficient(&[]), 1.0);

        // 趋势稳定
        let snapshots = vec![
            HistoricalSnapshot {
                commit_sha: "a".to_string(),
                commit_date: "2024-01-01".to_string(),
                base_risk_score: 50.0,
            },
            HistoricalSnapshot {
                commit_sha: "b".to_string(),
                commit_date: "2024-01-02".to_string(),
                base_risk_score: 50.0,
            },
        ];
        let trend = calculate_trend_coefficient(&snapshots);
        assert!((trend - 1.0).abs() < 0.01);

        // 趋势上升（风险增加）
        // historical_mean = (40 + 60) / 2 = 50
        // current = 60
        // trend_delta = (50 - 60) / 50 = -0.2
        // trend_coefficient = 1 - (-0.2) * 0.3 = 1 + 0.06 = 1.06
        let snapshots = vec![
            HistoricalSnapshot {
                commit_sha: "a".to_string(),
                commit_date: "2024-01-01".to_string(),
                base_risk_score: 40.0,
            },
            HistoricalSnapshot {
                commit_sha: "b".to_string(),
                commit_date: "2024-01-02".to_string(),
                base_risk_score: 60.0,
            },
        ];
        let trend = calculate_trend_coefficient(&snapshots);
        assert!(trend > 1.0); // 风险增加，系数应该大于 1（放大最终分数）

        // 趋势下降（风险降低）
        // historical_mean = (60 + 40) / 2 = 50
        // current = 40
        // trend_delta = (50 - 40) / 50 = 0.2
        // trend_coefficient = 1 - 0.2 * 0.3 = 1 - 0.06 = 0.94
        let snapshots = vec![
            HistoricalSnapshot {
                commit_sha: "a".to_string(),
                commit_date: "2024-01-01".to_string(),
                base_risk_score: 60.0,
            },
            HistoricalSnapshot {
                commit_sha: "b".to_string(),
                commit_date: "2024-01-02".to_string(),
                base_risk_score: 40.0,
            },
        ];
        let trend = calculate_trend_coefficient(&snapshots);
        assert!(trend < 1.0); // 风险降低，系数应该小于 1（缩小最终分数）

        // 范围限制
        assert!((0.91..=1.09).contains(&trend));
    }

    #[test]
    fn test_final_heat_score() {
        // 基础测试
        let score = calculate_final_heat_score(50.0, 1.0);
        assert_eq!(score, 50.0);

        // 趋势系数影响
        let score = calculate_final_heat_score(50.0, 1.05);
        assert!((score - 52.5).abs() < 0.01);

        // 限制在 [0, 100]
        let score = calculate_final_heat_score(150.0, 1.0);
        assert_eq!(score, 100.0);

        let score = calculate_final_heat_score(-10.0, 1.0);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_repo_total_heat_score() {
        let files = vec![
            create_test_file_score("a.rs", 100, 50.0),
            create_test_file_score("b.rs", 200, 70.0),
        ];

        let total = calculate_repo_total_heat_score(&files);

        // weighted_mean = (50 * 100/300) + (70 * 200/300) = 16.67 + 46.67 = 63.33
        // top_5pct_penalty = 70 * 0.2 = 14
        // total = 63.33 + 14 = 77.33
        assert!(total > 0.0 && total <= 100.0);
    }

    #[test]
    fn test_repo_total_heat_score_empty() {
        let files: Vec<FileScore> = vec![];
        let total = calculate_repo_total_heat_score(&files);
        assert_eq!(total, 0.0);
    }

    #[test]
    fn test_repo_total_heat_score_zero_loc() {
        let files = vec![create_test_file_score("a.rs", 0, 50.0)];
        let total = calculate_repo_total_heat_score(&files);
        assert_eq!(total, 0.0);
    }

    #[test]
    fn test_top_risk_files() {
        let files = vec![
            create_test_file_score("a.rs", 100, 30.0),
            create_test_file_score("b.rs", 100, 50.0),
            create_test_file_score("c.rs", 100, 70.0),
            create_test_file_score("d.rs", 100, 90.0),
            create_test_file_score("e.rs", 100, 10.0),
        ];

        let top = calculate_top_risk_files(&files);
        assert_eq!(top.len(), 1); // 5 * 0.05 = 0.25, ceil = 1
        assert_eq!(top[0].raw.path, "d.rs");
        assert_eq!(top[0].final_heat_score, 90.0);
    }

    #[test]
    fn test_top_risk_files_tiebreaker() {
        let files = vec![
            create_test_file_score("b.rs", 100, 50.0),
            create_test_file_score("a.rs", 100, 50.0),
            create_test_file_score("c.rs", 100, 50.0),
        ];

        let top = calculate_top_risk_files(&files);
        // 分数相同时应按路径字典序
        assert_eq!(top[0].raw.path, "a.rs");
    }

    /// 防刷分测试：函数拆分不显著改变分数（cli-spec §15）。
    ///
    /// 拆分行为会令每个文件的复杂度综合值同比例下降，但 percentile 是相对排名：
    /// 全仓库同比例下降后排序不变，base_risk_score 保持不变（变化 <= 15%）。
    #[test]
    fn test_anti_cheat_function_split() {
        let before = vec![
            DimensionValues {
                complexity_value: 20.0,
                history_value: 30.0,
                dependency_value: 10.0,
            },
            DimensionValues {
                complexity_value: 40.0,
                history_value: 30.0,
                dependency_value: 10.0,
            },
            DimensionValues {
                complexity_value: 60.0,
                history_value: 30.0,
                dependency_value: 10.0,
            },
        ];
        // 模拟全仓库函数拆分：复杂度综合值同比例减半
        let after: Vec<DimensionValues> = before
            .iter()
            .map(|d| DimensionValues {
                complexity_value: d.complexity_value * 0.5,
                history_value: d.history_value,
                dependency_value: d.dependency_value,
            })
            .collect();

        for i in 0..before.len() {
            let pb = calculate_percentile_scores(&before[i], &before);
            let pa = calculate_percentile_scores(&after[i], &after);
            let base_b = calculate_base_risk_score(&pb);
            let base_a = calculate_base_risk_score(&pa);
            let relative_change = (base_a - base_b).abs() / base_b.max(0.01);
            assert!(
                relative_change <= 0.15,
                "拆分后 base_risk 相对变化 {} 超过 15%",
                relative_change
            );
        }
    }

    fn create_test_file_score(path: &str, loc: u32, heat: f64) -> FileScore {
        FileScore {
            raw: FileRawMetrics {
                path: path.to_string(),
                language: Language::Rust,
                loc,
                cyclomatic_complexity: 10,
                max_if_nesting_depth: 2,
                nested_if_ratio: 0.3,
                avg_function_length: 20.0,
                max_function_length: 50,
                commit_count: 5,
                distinct_authors: 2,
                last_modified_days_ago: 30,
                incident_commit_count: 1,
                max_cve_severity: Severity::None,
                cve_count: 0,
                dependency_staleness: 0.0,
                dependency_data_incomplete: false,
                parse_incomplete: false,
            },
            percentiles: FilePercentileScores {
                complexity_risk: 50.0,
                history_risk: 50.0,
                dependency_risk: 50.0,
            },
            dimension_values: DimensionValues {
                complexity_value: 20.0,
                history_value: 30.0,
                dependency_value: 0.0,
            },
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
}
