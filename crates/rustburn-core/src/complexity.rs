//! 语言检测与复杂度指标入口。
//!
//! v2 起复杂度计算逻辑迁移到 [crate::analyzers::complexity::ComplexityAnalyzer]，
//! 本模块保留 [detect_language]（CLI 扫描用）并重新导出指标类型。

pub use crate::analyzers::complexity::FileComplexity;

use crate::model::Language;

/// 检测文件语言。
pub fn detect_language(path: &str) -> Language {
    let path_lower = path.to_lowercase();
    if path_lower.ends_with(".rs") {
        Language::Rust
    } else if path_lower.ends_with(".js") || path_lower.ends_with(".jsx") {
        Language::JavaScript
    } else if path_lower.ends_with(".go") {
        Language::Go
    } else {
        Language::Unknown
    }
}
