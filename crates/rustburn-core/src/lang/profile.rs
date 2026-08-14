//! 语言配置档案：把"测试约定"与"锁文件解析器"这两类语言特定知识，
//! 从 TestAnalyzer / DependencyAnalyzer 内部抽出来，做成按语言注册的配置表。
//!
//! 新增语言时，除实现 [crate::lang::LanguageAdapter]（lang/<name>.rs）之外，
//! 若该语言有特定的测试命名约定 / 覆盖率报告格式 / 锁文件格式，在这里注册
//! 即可，不需要改动任何 analyzer（SPEC v2 §9 架构解耦的延伸）。

use crate::dependency::Dependency;
use crate::model::Language;

/// 该语言的测试约定（TestAnalyzer 的语言特定配置来源）。
pub struct TestConventionConfig {
    /// 测试文件命名模式，`{name}` 为对应实现文件的主干名。
    pub test_file_patterns: &'static [&'static str],
    /// 覆盖率报告文件 glob（仓库根相对）。
    pub coverage_report_globs: &'static [&'static str],
}

/// 锁文件解析器：把语言特定的锁文件内容解析成 (模块, 版本) 列表供 OSV 查询。
pub trait LockfileParser: Send + Sync {
    /// 解析器名称（如 `go.sum`）。
    fn name(&self) -> &'static str;

    /// 该解析器处理的锁文件名列表。
    fn lockfile_names(&self) -> &'static [&'static str];

    /// 解析锁文件内容。
    fn parse(&self, content: &str) -> Vec<Dependency>;
}

/// 语言配置档案：按语言聚合测试约定与锁文件解析器。
pub struct LanguageProfile {
    /// 对应语言。
    pub language: Language,
    /// 测试约定。
    pub test_conventions: TestConventionConfig,
    /// 锁文件解析器列表。
    pub lockfile_parsers: &'static [&'static dyn LockfileParser],
}
