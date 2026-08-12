//! rustburn-report — HTML 报告生成。
//! 使用 askama 模板引擎生成技术债分析报告。

use std::fs;
use std::path::Path;

use anyhow::Result;
use askama::Template;
use rustburn_core::model::{HistoricalSnapshot, HistoryRewriteState, RepoReport, Severity};

/// HTML 报告模板
#[derive(Template)]
#[template(path = "report.html")]
struct ReportTemplate<'a> {
    report: &'a RepoReport,
    report_data_json: String,
}

#[allow(dead_code)]
impl<'a> ReportTemplate<'a> {
    /// 根据热度分数返回 CSS 类名
    fn score_class(&self, score: &f64) -> &'static str {
        if *score < 30.0 {
            "score-low"
        } else if *score < 60.0 {
            "score-medium"
        } else if *score < 80.0 {
            "score-high"
        } else {
            "score-critical"
        }
    }

    /// 根据热度分数返回风险等级文字
    fn risk_level(&self, score: &f64) -> &'static str {
        if *score < 30.0 {
            "低风险"
        } else if *score < 60.0 {
            "中风险"
        } else if *score < 80.0 {
            "高风险"
        } else {
            "严重风险"
        }
    }

    /// 根据严重度返回 CSS 类名
    fn severity_class(&self, severity: &Severity) -> &'static str {
        match *severity {
            Severity::Low => "score-low",
            Severity::Medium => "score-medium",
            Severity::High => "score-high",
            Severity::Critical => "score-critical",
            _ => "",
        }
    }

    /// 计算总代码行数
    fn total_loc(&self) -> u32 {
        self.report.files.iter().map(|f| f.raw.loc).sum()
    }

    /// 格式化 f64（通过引用，用于模板中的字段访问）
    fn fmt_f64(&self, value: &f64, precision: usize) -> String {
        format!("{:.prec$}", value, prec = precision)
    }

    /// 格式化 f64（通过值，用于模板中的方法返回值/表达式）
    fn fmt_f64v(&self, value: f64, precision: usize) -> String {
        format!("{:.prec$}", value, prec = precision)
    }

    /// 格式化总代码行数（带千分位）
    fn format_total_loc(&self) -> String {
        let s = self.total_loc().to_string();
        let mut result = String::new();
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                result.push(',');
            }
            result.push(c);
        }
        result.chars().rev().collect()
    }

    /// 格式化文件数量（带千分位，接受 &usize）
    fn format_number_usize(&self, value: &usize) -> String {
        let s = value.to_string();
        let mut result = String::new();
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                result.push(',');
            }
            result.push(c);
        }
        result.chars().rev().collect()
    }

    /// 计算平均置信度
    fn avg_confidence(&self) -> f64 {
        if self.report.files.is_empty() {
            return 0.0;
        }
        let sum: f64 = self
            .report
            .files
            .iter()
            .map(|f| f.consistency.coefficient)
            .sum();
        sum / self.report.files.len() as f64
    }

    /// 低置信度文件数量
    fn low_confidence_count(&self) -> usize {
        self.report
            .files
            .iter()
            .filter(|f| f.consistency.coefficient < 0.7)
            .count()
    }

    /// Top 5% 风险文件数量
    fn top_risk_count(&self) -> usize {
        self.report.top_risk_files.len()
    }

    /// Top 5% 风险文件占比百分比（格式化为字符串）
    fn top_risk_percentage_str(&self) -> String {
        format!("{:.1}", self.top_risk_percentage())
    }

    /// Top 5% 风险文件占比（f64）
    fn top_risk_percentage(&self) -> f64 {
        let file_count = self.report.analysis_metadata.file_count.max(1) as f64;
        (self.report.top_risk_files.len() as f64 / file_count) * 100.0
    }

    /// 计算依赖漏洞总数
    fn total_vulnerabilities(&self) -> usize {
        self.report.dependency_findings.len()
    }

    /// 历史重写检测是否确认为 Detected
    fn is_history_rewrite_detected(&self, state: &HistoryRewriteState) -> bool {
        matches!(state, HistoryRewriteState::Detected)
    }

    /// 历史重写检测是否确认为 NotDetected
    fn is_history_rewrite_not_detected(&self, state: &HistoryRewriteState) -> bool {
        matches!(state, HistoryRewriteState::NotDetected)
    }

    /// 计算趋势历史中的平均 base_risk_score
    fn avg_trend_history_risk(&self, history: &[HistoricalSnapshot]) -> f64 {
        if history.is_empty() {
            return 0.0;
        }
        let sum: f64 = history.iter().map(|s| s.base_risk_score).sum();
        sum / history.len() as f64
    }
}

/// 生成 HTML 报告
pub fn generate_html_report(report: &RepoReport) -> Result<String> {
    // 将报告数据序列化为 JSON，用于嵌入 HTML
    let report_data_json = serde_json::to_string(report)?;

    let template = ReportTemplate {
        report,
        report_data_json,
    };
    template
        .render()
        .map_err(|e| anyhow::anyhow!("渲染模板失败: {}", e))
}

/// 将报告写入文件
pub fn write_report(report: &RepoReport, output_path: &Path) -> Result<()> {
    let html = generate_html_report(report)?;
    fs::write(output_path, html)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustburn_core::model::{
        AnalysisMetadata, ConsistencyReport, FilePercentileScores, FileRawMetrics, FileScore,
        HistoryRewriteState, Language, RepoReport, Severity,
    };

    fn create_test_report() -> RepoReport {
        RepoReport {
            schema_version: "1.0".to_string(),
            rustburn_version: "0.1.0".to_string(),
            analysis_version: 1,
            repo_path: "/test/repo".to_string(),
            scanned_at: "2024-01-01T00:00:00Z".to_string(),
            files: vec![create_test_file_score("src/main.rs", 45.0)],
            repo_total_heat_score: 45.0,
            top_risk_files: vec![create_test_file_score("src/main.rs", 45.0)],
            dependency_findings: vec![],
            anomalies: vec![],
            analysis_metadata: AnalysisMetadata {
                max_commits: 5000,
                history_truncated: false,
                offline: false,
                osv_status: "success".to_string(),
                supported_languages: vec!["rust".to_string(), "javascript".to_string()],
                elapsed_seconds: 1.5,
                file_count: 1,
                skipped_symlinks: 0,
                skipped_files: 0,
            },
            warnings: vec![],
        }
    }

    fn create_test_file_score(path: &str, heat: f64) -> FileScore {
        FileScore {
            raw: FileRawMetrics {
                path: path.to_string(),
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
            },
            percentiles: FilePercentileScores {
                complexity_risk: 50.0,
                history_risk: 50.0,
                dependency_risk: 50.0,
            },
            dimension_values: rustburn_core::model::DimensionValues {
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

    #[test]
    fn test_generate_html_report() {
        let report = create_test_report();
        let html = generate_html_report(&report).unwrap();
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("src/main.rs"));
        assert!(html.contains("45.00"));
    }

    #[test]
    fn test_write_report() {
        let report = create_test_report();
        let temp_dir = tempfile::tempdir().unwrap();
        let output_path = temp_dir.path().join("report.html");

        write_report(&report, &output_path).unwrap();
        assert!(output_path.exists());

        let content = fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("<!DOCTYPE html>"));
    }
}
