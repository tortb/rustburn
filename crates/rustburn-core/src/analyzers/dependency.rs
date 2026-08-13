//! DependencyAnalyzer：依赖风险维度。
//!
//! 完全语言无关，只消费锁文件 + OSV 查询结果（已按 aliases 去重）。
//! risk_score 沿用原 SPEC 第 6 节逻辑：CVSS 严重度 0.6 + CVE 数量档位 0.25。
//!
//! 禁止事项 6-A：任何 CVE/漏洞记录必须直接来自 OSV API 的真实网络响应，
//! mock 数据只在测试中标注"测试桩数据"且不能出现在正常路径。
//! 禁止事项 6-B：跨数据源去重（GHSA/RUSTSEC 通过 aliases 关联）在
//! [crate::dependency] 中完成，同一漏洞只计 1 条。

use serde_json::json;

use crate::analyzer::DimensionAnalyzer;
use crate::context::{DependencyFileData, FileContext};
use crate::model::{Confidence, DimensionResult, Severity};

/// 依赖风险分：CVSS 严重度 0.6 + CVE 数量档位 0.25。
pub fn dependency_risk(data: &DependencyFileData) -> f64 {
    let severity_score = data.max_cve_severity.to_score();
    let cve_band = match data.cve_count {
        0 => 0.0,
        1 => 60.0,
        2..=4 => 80.0,
        _ => 100.0,
    };
    (severity_score * 0.6 + cve_band * 0.25).clamp(0.0, 100.0)
}

/// DependencyAnalyzer：只吃锁文件。
pub struct DependencyAnalyzer;

impl DimensionAnalyzer for DependencyAnalyzer {
    fn name(&self) -> &'static str {
        "dependency"
    }

    fn analyze(&self, ctx: &FileContext<'_>) -> DimensionResult {
        // 数据缺失（离线 / OSV 查询失败 / 文件语法解析不完整导致 import 不可靠）：
        // 用仓库均值填充，禁止硬编码 0/100。
        if ctx.dependency.data_incomplete || ctx.parse_incomplete {
            let risk = ctx.repo.dependency_risk_mean.unwrap_or(50.0);
            let reason = if ctx.dependency.data_incomplete {
                "依赖数据不完整（离线模式或 OSV 查询失败）".to_string()
            } else {
                "语法解析不完整，依赖引用无法可靠提取".to_string()
            };
            return DimensionResult {
                raw_value: risk,
                risk_score: risk,
                confidence: Confidence::DataMissing(reason),
                detail: json!({ "filled_with": "repo_mean" }),
            };
        }

        let risk = dependency_risk(ctx.dependency);
        DimensionResult {
            raw_value: risk,
            risk_score: risk,
            confidence: Confidence::Full,
            detail: json!({
                "max_cve_severity": severity_name(ctx.dependency.max_cve_severity),
                "cve_count": ctx.dependency.cve_count,
            }),
        }
    }
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::None => "none",
        Severity::Low => "low",
        Severity::Medium => "medium",
        Severity::High => "high",
        Severity::Critical => "critical",
    }
}
