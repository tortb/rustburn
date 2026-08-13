//! 分析上下文：连接五个 DimensionAnalyzer 与仓库级数据。
//!
//! [FileContext] 聚合单文件视图所需的全部数据（源码、git 时间线、
//! 依赖数据、测试统计、仓库级分布），各 Analyzer 只消费它需要的部分。

use std::collections::HashMap;

use crate::model::{Language, Severity};

/// 单个文件在 git 历史中的时间线（ChangeRiskAnalyzer 使用）。
///
/// 只保存 commit 的 UTC 时间戳（秒），时间基准由分析时的当前时间决定，
/// 不允许用文件首次出现时间作为衰减基准（SPEC v2 §5 禁止事项 5-B）。
#[derive(Debug, Clone, Default)]
pub struct GitTimeline {
    /// 该文件参与的全部 commit 时间戳（去重，同一 commit 只记一次）
    pub commit_timestamps: Vec<i64>,
    /// 该文件中 incident commit 的时间戳
    pub incident_timestamps: Vec<i64>,
    /// 不同作者数量
    pub distinct_authors: u32,
}

impl GitTimeline {
    /// 是否没有任何 commit 数据（此时 ChangeRiskAnalyzer 应标记数据缺失）。
    pub fn is_empty(&self) -> bool {
        self.commit_timestamps.is_empty()
    }
}

/// 该文件的依赖数据（DependencyAnalyzer 使用）。
#[derive(Debug, Clone, Default)]
pub struct DependencyFileData {
    /// 引用依赖中最高的 CVE 严重度
    pub max_cve_severity: Severity,
    /// 引用依赖涉及的漏洞数量
    pub cve_count: u32,
    /// 依赖数据是否不完整（离线模式 / OSV 查询失败）
    pub data_incomplete: bool,
}

/// 单个测试文件的统计（TestAnalyzer 使用）。
#[derive(Debug, Clone)]
pub struct TestFileStats {
    /// 测试文件相对路径
    pub path: String,
    /// 测试行数（内部 test mod 场景为 mod 内行数）
    pub test_loc: u32,
    /// 密度分母：实现文件用于对比的行数
    /// （外部测试文件 = 实现文件 LOC；内部 test mod = 文件 LOC - mod 行数）
    pub impl_loc: u32,
    /// 测试函数体内的断言调用数
    pub assertion_count: u32,
}

/// 仓库级测试上下文（TestAnalyzer 使用）。
#[derive(Debug, Clone, Default)]
pub struct TestRepoContext {
    /// 实现文件路径 → 对应测试文件统计列表
    pub test_files: HashMap<String, Vec<TestFileStats>>,
    /// 实现文件路径 → 覆盖率（0-100），来自 lcov/cobertura 报告
    pub coverage: HashMap<String, f64>,
    /// 有覆盖率数据的文件其覆盖率缺口均值（部分缺失时的填充值）
    pub mean_coverage_gap: Option<f64>,
}

/// 仓库级分析数据（跨文件共享）。
#[derive(Debug, Clone, Default)]
pub struct RepoAnalysisData {
    /// 全部文件的复杂度原始值（ComplexityAnalyzer 算百分位用）
    pub complexity_raw_values: Vec<f64>,
    /// 复杂度风险均值（语法解析失败文件的填充值）
    pub complexity_risk_mean: Option<f64>,
    /// 重复度风险均值（语法解析失败文件的填充值）
    pub duplication_risk_mean: Option<f64>,
    /// 文件路径 → 该文件参与"重复组"的行区间（由仓库级结构哈希分组预计算）
    pub duplication_line_ranges: HashMap<String, Vec<(u32, u32)>>,
    /// 测试统计
    pub test: TestRepoContext,
    /// 依赖风险均值（有完整数据文件的 risk_score 均值，DataMissing 填充用）
    pub dependency_risk_mean: Option<f64>,
    /// 变更风险均值（有 commit 数据文件的 risk_score 均值，DataMissing 填充用）
    pub change_risk_mean: Option<f64>,
}

/// 单文件分析上下文。
pub struct FileContext<'a> {
    /// 文件相对路径（仓库根相对，/ 分隔）
    pub path: &'a str,
    /// 源码
    pub source: &'a str,
    /// 语言
    pub language: Language,
    /// 有效代码行数
    pub loc: u32,
    /// 语法解析是否不完整
    pub parse_incomplete: bool,
    /// 预解析的语法树（解析失败时为 None）
    pub tree: Option<&'a tree_sitter::Tree>,
    /// 语言适配器（分析器只通过它接触语言细节）
    pub adapter: &'a dyn crate::lang::LanguageAdapter,
    /// 该文件的 git 时间线
    pub git: &'a GitTimeline,
    /// 该文件的依赖数据
    pub dependency: &'a DependencyFileData,
    /// 仓库级数据
    pub repo: &'a RepoAnalysisData,
}
