//! 维度分析器 trait（v2 架构）。
//!
//! 五个打分角色互相独立，均不感知语言细节：语言相关逻辑一律通过
//! [crate::lang::LanguageAdapter] 暴露。

use crate::context::FileContext;
use crate::model::DimensionResult;

/// 维度分析器：每个维度一个实现，输入 [FileContext]，输出 [DimensionResult]。
///
/// SPEC v2 §1.1：严格照此实现，不允许简化签名。
pub trait DimensionAnalyzer: Send + Sync {
    /// 维度名称（complexity / duplication / test / change_risk / dependency）
    fn name(&self) -> &'static str;

    /// 分析单个文件，产出该维度的独立结果。
    fn analyze(&self, ctx: &FileContext<'_>) -> DimensionResult;
}
