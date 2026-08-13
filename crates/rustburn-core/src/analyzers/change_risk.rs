//! ChangeRiskAnalyzer：变更风险维度（替代原 history 维度）。
//!
//! 完全语言无关，只吃 git 时间线。
//!
//! 公式（SPEC v2 §5，必须逐字实现）：
//! ```text
//! change_risk_value = 近期事故密度 × 0.6 + 近期改动频率 × 0.2 + 作者分散度 × 0.2
//!
//! 近期事故密度 = Σ(每次incident commit的衰减权重) / Σ(同期全部commit的衰减权重)
//!               衰减权重 = 0.5 ^ (距今月数 / 6)   # 半衰期 6 个月
//! 近期改动频率 = min(100, 最近90天commit数 / 90 × 100)
//! 作者分散度   = min(100, distinct_authors × 10)
//! ```
//!
//! 禁止事项 5-A：公式任何一环不得使用终身累计值（commit_count /
//! incident_commit_count 不经衰减参与计算）。本实现只消费 [crate::context::GitTimeline]
//! 中的时间戳，并在计算前先做衰减加权。
//! 禁止事项 5-B：时间基准为"分析时的当前时间"（调用方传入），不允许用文件
//! 首次出现时间作为基准。

use chrono::Utc;
use serde_json::json;

use crate::analyzer::DimensionAnalyzer;
use crate::context::FileContext;
use crate::model::{Confidence, DimensionResult};

/// 一个月的秒数近似值。
const MONTH_SECS: f64 = 30.44 * 86400.0;
/// 近期窗口：90 天。
const RECENT_WINDOW_SECS: i64 = 90 * 86400;

/// 衰减权重：0.5 ^ (距今月数 / 6)。
fn decay_weight(now: i64, commit_time: i64) -> f64 {
    let months_ago = (now - commit_time).max(0) as f64 / MONTH_SECS;
    0.5_f64.powf(months_ago / 6.0)
}

/// 计算变更风险值（0-100）。
///
/// `now` 为分析时的当前时间戳（UTC 秒），作为衰减基准点。
pub fn change_risk_value(timeline: &crate::context::GitTimeline, now: i64) -> f64 {
    // 近期事故密度：衰减加权的 incident / 全部 commit（两者同源同期，保证长期不单调上涨）
    let incident_decay: f64 = timeline
        .incident_timestamps
        .iter()
        .map(|&ts| decay_weight(now, ts))
        .sum();
    let all_decay: f64 = timeline
        .commit_timestamps
        .iter()
        .map(|&ts| decay_weight(now, ts))
        .sum();
    let incident_density = if all_decay > 0.0 {
        incident_decay / all_decay
    } else {
        0.0
    };

    // 近期改动频率：最近 90 天 commit 数（窗口统计，不累计终身值）
    let recent_commits = timeline
        .commit_timestamps
        .iter()
        .filter(|&&ts| now - ts <= RECENT_WINDOW_SECS)
        .count() as f64;
    let recent_frequency = (recent_commits / 90.0 * 100.0).min(100.0);

    // 作者分散度
    let author_dispersion = (timeline.distinct_authors as f64 * 10.0).min(100.0);

    (incident_density * 100.0 * 0.6 + recent_frequency * 0.2 + author_dispersion * 0.2)
        .clamp(0.0, 100.0)
}

/// ChangeRiskAnalyzer：完全语言无关，只吃 git log。
pub struct ChangeRiskAnalyzer;

impl DimensionAnalyzer for ChangeRiskAnalyzer {
    fn name(&self) -> &'static str {
        "change_risk"
    }

    fn analyze(&self, ctx: &FileContext<'_>) -> DimensionResult {
        let now = Utc::now().timestamp();

        // 无任何 commit 数据：数据缺失，用仓库均值填充（禁止硬编码 0/100）
        if ctx.git.is_empty() {
            let risk = ctx.repo.change_risk_mean.unwrap_or(50.0);
            return DimensionResult {
                raw_value: risk,
                risk_score: risk,
                confidence: Confidence::DataMissing("该文件无 git commit 数据".to_string()),
                detail: json!({ "reason": "无 commit 时间线" }),
            };
        }

        let risk = change_risk_value(ctx.git, now);
        let (incident_density, recent_frequency, author_dispersion) = risk_components(ctx.git, now);
        DimensionResult {
            raw_value: risk,
            risk_score: risk,
            confidence: Confidence::Full,
            detail: json!({
                "incident_density": incident_density,
                "recent_frequency": recent_frequency,
                "author_dispersion": author_dispersion,
                "total_commits_in_window": ctx.git.commit_timestamps.len(),
            }),
        }
    }
}

/// 分解三个子项（供 detail 展示）。
fn risk_components(timeline: &crate::context::GitTimeline, now: i64) -> (f64, f64, f64) {
    let incident_decay: f64 = timeline
        .incident_timestamps
        .iter()
        .map(|&ts| decay_weight(now, ts))
        .sum();
    let all_decay: f64 = timeline
        .commit_timestamps
        .iter()
        .map(|&ts| decay_weight(now, ts))
        .sum();
    let incident_density = if all_decay > 0.0 {
        incident_decay / all_decay * 100.0
    } else {
        0.0
    };
    let recent_commits = timeline
        .commit_timestamps
        .iter()
        .filter(|&&ts| now - ts <= RECENT_WINDOW_SECS)
        .count() as f64;
    let recent_frequency = (recent_commits / 90.0 * 100.0).min(100.0);
    let author_dispersion = (timeline.distinct_authors as f64 * 10.0).min(100.0);
    (incident_density, recent_frequency, author_dispersion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::GitTimeline;

    fn timeline(commits: &[(i64, bool)], authors: u32) -> GitTimeline {
        let mut t = GitTimeline::default();
        for (ts, is_incident) in commits {
            t.commit_timestamps.push(*ts);
            if *is_incident {
                t.incident_timestamps.push(*ts);
            }
        }
        t.distinct_authors = authors;
        t
    }

    const MONTH: i64 = 30 * 86400;

    /// SPEC v2 §5 核心验收：时间衰减必须让"历史事故密集但近期稳定"的文件风险显著下降。
    ///
    /// 快照 A：3 年前 commit 密集、含 2 次 incident；
    /// 快照 B：在 A 基础上追加"最近 12 个月内只有 3 次 commit、0 次 incident"。
    /// 断言：B 的 change_risk_value 必须显著低于 A。
    #[test]
    fn test_decay_prevents_monotonic_increase() {
        let now = 1_800_000_000i64; // 模拟分析时的当前时间
        let three_years_ago = now - 36 * MONTH;
        let six_months_ago = now - 6 * MONTH;

        // 快照 A：3 年前的密集历史，10 次 commit，其中 2 次 incident
        let mut a_commits: Vec<(i64, bool)> = Vec::new();
        for i in 0..10 {
            a_commits.push((three_years_ago + i * 86400, i < 2));
        }
        let a = timeline(&a_commits, 3);
        let a_risk = change_risk_value(&a, now);

        // 快照 B：A 的完整历史 + 最近 6 个月 3 次 commit、0 次 incident
        let mut b_commits = a_commits.clone();
        for i in 0..3 {
            b_commits.push((six_months_ago + i * 86400, false));
        }
        let b = timeline(&b_commits, 3);
        let b_risk = change_risk_value(&b, now);

        assert!(
            b_risk < a_risk * 0.6,
            "衰减机制必须让 B 显著低于 A（A={:.2} B={:.2}）",
            a_risk,
            b_risk
        );
        assert!(
            b_risk < 30.0,
            "近期无事故的 B 不应仍处于高风险（B={:.2}）",
            b_risk
        );
    }

    /// 禁止事项 5-A：公式不得使用终身累计值——事故密度是同源衰减加权比值，
    /// 只增加旧 incident 而不增加近期 commit 时，风险不会单调上涨。
    #[test]
    fn test_old_incidents_fade_over_time() {
        let now = 1_800_000_000i64;
        let long_ago = now - 60 * MONTH;

        // 一份"全部事故都在很久以前"的历史
        let mut old: Vec<(i64, bool)> = Vec::new();
        for i in 0..20 {
            old.push((long_ago + i * 86400, i % 5 == 0)); // 4 次 incident
        }
        let old_tl = timeline(&old, 2);
        let old_risk = change_risk_value(&old_tl, now);

        // 同一份历史 + 近期持续正常提交（稀释事故密度）
        let mut calm: Vec<(i64, bool)> = old.clone();
        for i in 0..30 {
            calm.push((now - i * 86400, false)); // 最近 30 天密集正常提交
        }
        let calm_tl = timeline(&calm, 2);
        let calm_risk = change_risk_value(&calm_tl, now);

        assert!(
            calm_risk < old_risk,
            "近期正常提交应稀释事故密度（old={:.2} calm={:.2}）",
            old_risk,
            calm_risk
        );
    }

    /// 无 commit 数据时（空时间线）风险由上层标记 DataMissing，此处验证公式不 panic。
    #[test]
    fn test_empty_timeline_returns_finite() {
        let now = 1_800_000_000i64;
        let empty = GitTimeline::default();
        let risk = change_risk_value(&empty, now);
        assert!(risk.is_finite());
        assert_eq!(risk, 0.0);
    }
}
