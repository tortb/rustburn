use serde::{Deserialize, Serialize};
use std::fmt;

/// 编程语言枚举。
/// JSON 输出使用稳定的小写字符串，禁止使用 Rust 默认 Debug 输出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    JavaScript,
    Unknown,
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::JavaScript => write!(f, "javascript"),
            Language::Unknown => write!(f, "unknown"),
        }
    }
}

/// 严重度级别。数值越大 = 风险越高。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    None,
    Low,
    Medium,
    High,
    Critical,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::None => write!(f, "none"),
            Severity::Low => write!(f, "low"),
            Severity::Medium => write!(f, "medium"),
            Severity::High => write!(f, "high"),
            Severity::Critical => write!(f, "critical"),
        }
    }
}

impl Severity {
    /// 将 severity 映射为分数（None=0, Low=25, Medium=50, High=75, Critical=100）
    pub fn to_score(self) -> f64 {
        match self {
            Severity::None => 0.0,
            Severity::Low => 25.0,
            Severity::Medium => 50.0,
            Severity::High => 75.0,
            Severity::Critical => 100.0,
        }
    }
}

/// OSV CVSS 严重度映射。
/// >= 9.0 → Critical, >= 7.0 → High, >= 4.0 → Medium, < 4.0 → Low
/// > 如果没有 CVSS 分数，使用 Medium 并设置 severity_estimated = true。
pub fn cvss_to_severity(cvss_score: Option<f64>) -> (Severity, bool) {
    match cvss_score {
        Some(s) if s >= 9.0 => (Severity::Critical, false),
        Some(s) if s >= 7.0 => (Severity::High, false),
        Some(s) if s >= 4.0 => (Severity::Medium, false),
        Some(_) => (Severity::Low, false),
        None => (Severity::Medium, true),
    }
}

/// 历史重写检测状态。
/// 不可简化为 bool，必须支持 unknown 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRewriteState {
    NotDetected,
    Detected,
    Unknown,
}

/// 异常标记类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "detail")]
pub enum AnomalyFlag {
    HistoryRewrite,
    TemporaryComplexityDrop,
    SuspiciousTrend,
    IncompleteDependencyData,
    IncompleteHistory,
}

impl fmt::Display for AnomalyFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnomalyFlag::HistoryRewrite => write!(f, "疑似历史重写"),
            AnomalyFlag::TemporaryComplexityDrop => write!(f, "临时复杂度下降"),
            AnomalyFlag::SuspiciousTrend => write!(f, "可疑趋势"),
            AnomalyFlag::IncompleteDependencyData => write!(f, "依赖数据不完整"),
            AnomalyFlag::IncompleteHistory => write!(f, "历史不完整"),
        }
    }
}

/// 文件的原始指标。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRawMetrics {
    /// 文件路径（仓库根相对路径，使用 / 分隔符）
    pub path: String,
    /// 编程语言
    pub language: Language,
    /// 有效代码行数（不含空行和纯注释行）
    pub loc: u32,
    /// 圈复杂度（所有函数平均值，四舍五入到整数）
    pub cyclomatic_complexity: u32,
    /// if 嵌套最大深度
    pub max_if_nesting_depth: u32,
    /// 嵌套 if 比例 = nested_if_count / total_if_count（total_if_count==0 时为 0）
    pub nested_if_ratio: f64,
    /// 平均函数长度
    pub avg_function_length: f64,
    /// 最大函数长度
    pub max_function_length: u32,
    /// 该文件出现在 commit diff 中的不同 commit 数量
    pub commit_count: u32,
    /// 不同作者数量
    pub distinct_authors: u32,
    /// 最近修改距今的天数（完整自然日，UTC）
    pub last_modified_days_ago: u32,
    /// incident commit 数量（fix/bug/revert/hotfix/patch/error/crash）
    pub incident_commit_count: u32,
    /// 该文件引用依赖中最高的 CVE 严重度
    pub max_cve_severity: Severity,
    /// 该文件引用依赖涉及的漏洞数量
    pub cve_count: u32,
    /// 依赖过时程度（0 表示无数据或不过时）
    pub dependency_staleness: f64,
    /// 是否缺少依赖数据
    #[serde(default)]
    pub dependency_data_incomplete: bool,
    /// 是否语法解析不完整
    #[serde(default)]
    pub parse_incomplete: bool,
}

/// 文件在各维度的 percentile 分数。
/// 范围 0.0..=100.0，0 = 风险最低，100 = 风险最高。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePercentileScores {
    pub complexity_risk: f64,
    pub history_risk: f64,
    pub dependency_risk: f64,
}

/// 历史快照（某个 commit 时刻的评分）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoricalSnapshot {
    /// 完整 commit SHA
    pub commit_sha: String,
    /// commit 日期（UTC）
    pub commit_date: String,
    /// 该历史状态重新计算得到的基础风险分数
    pub base_risk_score: f64,
}

/// 一致性报告。coefficient 仅用于 confidence，不参与 final_heat_score。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyReport {
    /// 覆盖率报告是否过期
    pub coverage_report_stale: bool,
    /// 历史重写状态
    pub history_rewrite: HistoryRewriteState,
    /// lockfile 是否不匹配
    pub lockfile_mismatch: bool,
    /// 置信度系数
    pub coefficient: f64,
}

/// 单文件完整评分。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileScore {
    /// 原始指标
    pub raw: FileRawMetrics,
    /// percentile 分数
    pub percentiles: FilePercentileScores,
    /// 维度综合值（用于报告透明度）
    pub dimension_values: DimensionValues,
    /// 基础风险分数
    pub base_risk_score: f64,
    /// 一致性报告
    pub consistency: ConsistencyReport,
    /// 趋势系数
    pub trend_coefficient: f64,
    /// 最终热度分数
    pub final_heat_score: f64,
    /// 趋势历史快照
    pub trend_history: Vec<HistoricalSnapshot>,
}

/// 依赖发现记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyFinding {
    /// CVE / GHSA / OSV / 其他 identifier
    pub id: String,
    /// 依赖包名
    pub package_name: String,
    /// 生态系统（crates.io / npm）
    pub ecosystem: String,
    /// 版本
    pub version: String,
    /// 严重度
    pub severity: Severity,
    /// severity 是否为估算（无 CVSS 时）
    pub severity_estimated: bool,
    /// 简要描述
    pub summary: String,
    /// 受影响的文件列表
    pub affected_files: Vec<String>,
}

/// 分析元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisMetadata {
    /// max commits 限制
    pub max_commits: u32,
    /// 历史是否被截断
    pub history_truncated: bool,
    /// 是否离线模式
    pub offline: bool,
    /// OSV 查询状态
    pub osv_status: String,
    /// 支持的语言列表
    pub supported_languages: Vec<String>,
    /// 分析耗时（秒）
    pub elapsed_seconds: f64,
    /// 扫描的文件数量
    pub file_count: usize,
    /// 文件数低于样本量阈值，百分位排名统计噪声较大（报告需显著标注）
    #[serde(default)]
    pub sample_size_warning: bool,
    /// 跳过的符号链接数量
    pub skipped_symlinks: usize,
    /// 跳过的二进制/不可解析文件数量
    pub skipped_files: usize,
}

/// 仓库级别报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoReport {
    /// schema 版本
    pub schema_version: String,
    /// rustburn 版本
    pub rustburn_version: String,
    /// 分析版本
    pub analysis_version: u32,
    /// 仓库路径
    pub repo_path: String,
    /// 扫描时间
    pub scanned_at: String,
    /// 所有文件的评分
    pub files: Vec<FileScore>,
    /// 仓库总热度分数
    pub repo_total_heat_score: f64,
    /// 风险最高的文件（Top 5%）
    pub top_risk_files: Vec<FileScore>,
    /// 依赖发现列表
    pub dependency_findings: Vec<DependencyFinding>,
    /// 异常列表
    pub anomalies: Vec<AnomalyFlag>,
    /// 分析元数据
    pub analysis_metadata: AnalysisMetadata,
    /// 警告信息
    pub warnings: Vec<String>,
}

/// 维度综合值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionValues {
    /// 复杂度综合值
    pub complexity_value: f64,
    /// 历史综合值
    pub history_value: f64,
    /// 依赖综合值
    pub dependency_value: f64,
}

/// 严重度分数映射。
#[derive(Debug, Clone, Copy)]
pub enum DependencySeverity {
    None = 0,
    Low = 25,
    Medium = 50,
    High = 75,
    Critical = 100,
}

/// 扫描配置。
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub repo_path: String,
    pub output: String,
    pub max_commits: u32,
    pub offline: bool,
    pub format: OutputFormat,
    pub fail_above: Option<f64>,
    pub ignore_patterns: Vec<String>,
}

/// 输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Html,
    Json,
}
