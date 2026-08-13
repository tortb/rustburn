//! 五个维度分析器（v2 架构）。
//!
//! 互相独立、不感知语言细节。新增语言时本目录必须零改动（SPEC v2 §9）。

pub mod change_risk;
pub mod complexity;
pub mod dependency;
pub mod duplication;
pub mod test;
