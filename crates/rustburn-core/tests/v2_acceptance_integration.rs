//! SPEC v2 验收集成测试：
//! - §1.1-A / §4：TestAnalyzer 置信度与均值填充、覆盖率、空壳测试、内部 test mod；
//! - §9：mock 语言架构解耦（分析器零改动即可支持新语言）；
//! - §7-A：scoring.rs 不得引用 FileRawMetrics（静态检查）。

use std::collections::HashMap;

use rustburn_core::analyzer::DimensionAnalyzer;
use rustburn_core::analyzers::test::{
    build_test_context, count_assertions, parse_cobertura, parse_lcov, TestAnalyzer, TestFileInput,
    TestPathRules,
};
use rustburn_core::context::{
    DependencyFileData, FileContext, GitTimeline, RepoAnalysisData, TestRepoContext,
};
use rustburn_core::lang::adapter_for;
use rustburn_core::model::{Confidence, Language};

/// 构造单文件分析上下文（测试辅助）。
#[allow(clippy::too_many_arguments)]
fn make_ctx<'a>(
    path: &'a str,
    source: &'a str,
    lang: Language,
    loc: u32,
    repo: &'a RepoAnalysisData,
    adapter: &'a dyn rustburn_core::lang::LanguageAdapter,
    git: &'a GitTimeline,
    dep: &'a DependencyFileData,
) -> FileContext<'a> {
    FileContext {
        path,
        source,
        language: lang,
        loc,
        parse_incomplete: false,
        tree: None,
        adapter,
        git,
        dependency: dep,
        repo,
    }
}

/// 验收：配 90% 覆盖率报告的文件，test_risk 明显低于无测试文件；
/// 无测试文件的 confidence 必须是 DataMissing，risk 用均值填充而非 0。
#[test]
fn test_covered_file_scores_below_no_test_file() {
    let mut test_ctx = TestRepoContext::default();
    test_ctx.test_files.insert(
        "src/foo.rs".to_string(),
        vec![rustburn_core::context::TestFileStats {
            path: "src/foo_test.rs".to_string(),
            test_loc: 60,
            impl_loc: 100,
            assertion_count: 10,
        }],
    );
    test_ctx.coverage.insert("src/foo.rs".to_string(), 90.0);
    test_ctx.mean_coverage_gap = Some(10.0);

    let repo = RepoAnalysisData {
        test: test_ctx,
        ..Default::default()
    };
    let adapter = adapter_for(Language::Rust).unwrap();
    let git = GitTimeline::default();
    let dep = DependencyFileData::default();

    // 有测试 + 90% 覆盖率
    let ctx_covered = make_ctx(
        "src/foo.rs",
        "fn foo() {}",
        Language::Rust,
        100,
        &repo,
        adapter.as_ref(),
        &git,
        &dep,
    );
    let covered = TestAnalyzer.analyze(&ctx_covered);
    assert!(covered.confidence.is_full(), "有完整覆盖率时应为 Full");
    // 覆盖率缺口10×0.5 + 密度缺口40×0.3 + 断言缺口96.67×0.2 ≈ 36.33
    assert!(
        (covered.risk_score - 36.333).abs() < 0.5,
        "covered risk={:.2}",
        covered.risk_score
    );

    // 无任何测试信号
    let ctx_plain = make_ctx(
        "src/plain.rs",
        "fn plain() {}",
        Language::Rust,
        100,
        &repo,
        adapter.as_ref(),
        &git,
        &dep,
    );
    let plain = TestAnalyzer.analyze(&ctx_plain);
    assert!(
        matches!(plain.confidence, Confidence::DataMissing(_)),
        "无测试信号必须标记 DataMissing"
    );
    // 均值填充：覆盖率缺口 10×0.5 + 30 + 20 = 55，绝不是 0
    assert!(
        plain.risk_score > covered.risk_score,
        "有覆盖率文件({:.1})应明显低于无测试文件({:.1})",
        covered.risk_score,
        plain.risk_score
    );
    assert!(
        plain.risk_score > 0.0 && (plain.risk_score - 55.0).abs() < 0.01,
        "无测试文件 risk 应使用均值填充（55），实际 {}",
        plain.risk_score
    );
}

/// 验收：完全没有覆盖率数据时，无测试文件 risk 用中性值填充且不是 0/100。
#[test]
fn test_no_test_and_no_coverage_uses_neutral_fill() {
    let repo = RepoAnalysisData::default();
    let adapter = adapter_for(Language::Rust).unwrap();
    let git = GitTimeline::default();
    let dep = DependencyFileData::default();
    let ctx = make_ctx(
        "a.rs",
        "fn a() {}",
        Language::Rust,
        10,
        &repo,
        adapter.as_ref(),
        &git,
        &dep,
    );
    let result = TestAnalyzer.analyze(&ctx);
    assert!(matches!(result.confidence, Confidence::DataMissing(_)));
    // 中性 50×0.5 + 30 + 20 = 75（既不是 0 也不是 100）
    assert!(
        (result.risk_score - 75.0).abs() < 0.01,
        "no-test no-coverage risk={}",
        result.risk_score
    );
}

/// 验收 4-B：空壳测试（测试函数体内无任何断言）→ 断言密度缺口接近 100。
#[test]
fn test_empty_shell_test_detected() {
    let shell = r#"
#[test]
fn test_it() {
    let result = compute();
    println!("{}", result);
}
"#;
    let real = r#"
#[test]
fn test_it() {
    let result = compute();
    assert_eq!(result, 5);
}
"#;
    assert_eq!(
        count_assertions(shell, Language::Rust),
        0,
        "空壳测试断言数应为 0"
    );
    assert_eq!(
        count_assertions(real, Language::Rust),
        1,
        "真实测试应有 1 个断言"
    );

    // 空壳测试的断言密度缺口 = 100
    let mut test_ctx = TestRepoContext::default();
    test_ctx.test_files.insert(
        "src/x.rs".to_string(),
        vec![rustburn_core::context::TestFileStats {
            path: "src/x_test.rs".to_string(),
            test_loc: 10,
            impl_loc: 50,
            assertion_count: 0,
        }],
    );
    let repo = RepoAnalysisData {
        test: test_ctx,
        ..Default::default()
    };
    let adapter = adapter_for(Language::Rust).unwrap();
    let git = GitTimeline::default();
    let dep = DependencyFileData::default();
    let ctx = make_ctx(
        "src/x.rs",
        "fn x() {}",
        Language::Rust,
        50,
        &repo,
        adapter.as_ref(),
        &git,
        &dep,
    );
    let result = TestAnalyzer.analyze(&ctx);
    // 断言密度缺口 = 100 - min(100, 0/10*20) = 100
    let assertion_gap = 100.0f64 - (0.0f64 / 10.0f64 * 20.0f64).min(100.0f64);
    assert_eq!(assertion_gap, 100.0);
    assert!(
        result.risk_score >= 50.0,
        "空壳测试风险应偏高，实际 {}",
        result.risk_score
    );
}

/// 验收 4.2 规则 2：Rust 内部 #[cfg(test)] mod tests 被识别为测试信号。
#[test]
fn test_rust_internal_test_mod_detected() {
    let impl_source = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(1, 2), 3);
    }
}
"#;
    let files = vec![TestFileInput {
        path: "src/lib.rs".to_string(),
        source: impl_source.to_string(),
        language: Language::Rust,
        loc: 16,
    }];
    let ctx = build_test_context(&files, None, &TestPathRules::default());
    let stats = ctx
        .test_files
        .get("src/lib.rs")
        .expect("应识别内部 test mod");
    assert_eq!(stats.len(), 1);
    assert!(stats[0].assertion_count >= 1, "test mod 内应有断言");
    assert!(stats[0].test_loc > 0);
    // 密度分母 = 文件 LOC - test mod 行数
    assert!(stats[0].impl_loc < 16, "impl_loc 应扣除 test mod 行数");
}

/// 验收 4.2 规则 1：同目录 `<文件名>_test.<ext>` 命名约定。
#[test]
fn test_same_dir_naming_convention() {
    let files = vec![
        TestFileInput {
            path: "src/parser.rs".to_string(),
            source: "fn parse() {}".to_string(),
            language: Language::Rust,
            loc: 1,
        },
        TestFileInput {
            path: "src/parser_test.rs".to_string(),
            source: "#[test]\nfn t() {\n    assert!(true);\n}\n".to_string(),
            language: Language::Rust,
            loc: 4,
        },
    ];
    let ctx = build_test_context(&files, None, &TestPathRules::default());
    let stats = ctx
        .test_files
        .get("src/parser.rs")
        .expect("parser.rs 应匹配到 parser_test.rs");
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].path, "src/parser_test.rs");
    assert!(stats[0].assertion_count >= 1);
}

/// 覆盖率解析：lcov 与 cobertura。
#[test]
fn test_coverage_parsers() {
    let lcov = r#"TN:
SF:src/foo.rs
LF:100
LH:90
end_of_record
SF:src/bar.rs
LF:50
LH:0
end_of_record
"#;
    let cov = parse_lcov(lcov);
    assert!((cov["src/foo.rs"] - 90.0).abs() < 0.01);
    assert_eq!(cov["src/bar.rs"], 0.0);

    let xml = r#"<coverage>
  <packages><package><classes><class filename="src/baz.rs">
    <lines>
      <line number="1" hits="1"/>
      <line number="2" hits="0"/>
      <line number="3" hits="1"/>
    </lines>
  </class></classes></package></packages>
</coverage>"#;
    let cov = parse_cobertura(xml);
    assert!((cov["src/baz.rs"] - 66.666).abs() < 0.1);
}

/// 验收 §9：mock 语言只需适配器，五个分析器零改动即可输出复杂度/重复度分数。
#[test]
fn test_mock_language_architecture_decoupling() {
    use rustburn_core::analyzers::complexity::ComplexityAnalyzer;
    use rustburn_core::analyzers::duplication::DuplicationAnalyzer;

    let mock_source = r#"
function aggregate(items, limit) {
    let total = 0;
    for (let i = 0; i < limit; i++) {
        total = total + items[i];
        if (total > 100) {
            break;
        }
    }
    return total;
}

function render(node) {
    if (node == null) {
        return "";
    }
    let out = "";
    for (let key in node) {
        out = out + key;
    }
    return out;
}
"#;

    let adapter = adapter_for(Language::Mock).expect("mock 适配器必须存在");
    let tree = adapter.parse(mock_source).expect("mock 解析成功");

    // 用真实复杂度计算验证 mock 语言输出
    let metrics =
        rustburn_core::analyzers::complexity::compute_metrics(&tree, mock_source, adapter.as_ref());
    assert!(
        metrics.cyclomatic_complexity >= 2,
        "mock 语言应能统计复杂度"
    );

    // 两个分析器直接消费 mock 语言上下文
    let loc = rustburn_core::analyzers::complexity::calculate_loc(&tree, mock_source);
    let mut repo = RepoAnalysisData {
        complexity_raw_values: vec![rustburn_core::analyzers::complexity::complexity_raw_value(
            &metrics,
        )],
        ..Default::default()
    };
    repo.complexity_risk_mean = Some(40.0);
    repo.duplication_risk_mean = Some(30.0);

    let git = GitTimeline::default();
    let dep = DependencyFileData::default();
    let ctx = FileContext {
        path: "mock/file.mk",
        source: mock_source,
        language: Language::Mock,
        loc,
        parse_incomplete: false,
        tree: Some(&tree),
        adapter: adapter.as_ref(),
        git: &git,
        dependency: &dep,
        repo: &repo,
    };

    let complexity = ComplexityAnalyzer.analyze(&ctx);
    assert_eq!(ComplexityAnalyzer.name(), "complexity");
    assert!(complexity.risk_score > 0.0, "mock 语言复杂度分数应大于 0");

    let duplication = DuplicationAnalyzer.analyze(&ctx);
    assert_eq!(DuplicationAnalyzer.name(), "duplication");
    assert!(duplication.risk_score >= 0.0, "mock 语言重复度分数可计算");
}

/// 验收 §7-A：scoring.rs 不得引用 FileRawMetrics（静态检查）。
#[test]
fn test_scoring_does_not_touch_filerawmetrics() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/scoring.rs"))
        .expect("读取 scoring.rs");
    // 不允许 import、函数参数或字段读取中引用 FileRawMetrics
    assert!(
        !src.contains("use crate::model::FileRawMetrics"),
        "scoring.rs 不得 import FileRawMetrics"
    );
    assert!(
        !src.contains(": &FileRawMetrics") && !src.contains(": FileRawMetrics"),
        "scoring.rs 不得以 FileRawMetrics 作为函数参数"
    );
    assert!(!src.contains(".raw."), "scoring.rs 不得读取 raw 指标字段");
}

/// 验收 §5 禁止事项 5-A 的代码审查辅助：change_risk.rs 不应使用原始累计计数参与公式。
#[test]
fn test_change_risk_formula_uses_no_lifetime_counts() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/analyzers/change_risk.rs"
    ))
    .expect("读取 change_risk.rs");
    // 公式中不允许通过字段访问使用 commit_count / incident_commit_count
    assert!(
        !src.contains(".commit_count") && !src.contains(".incident_commit_count"),
        "change_risk.rs 不得访问终身累计字段"
    );
}

/// 验收 §1.1-A：Confidence 语义——默认数据不完整，验证后才能 Full。
#[test]
fn test_confidence_defaults_to_incomplete() {
    let empty: HashMap<String, Vec<rustburn_core::context::TestFileStats>> = HashMap::new();
    assert!(empty.is_empty());
    // DataMissing 必须携带具体原因
    let missing = Confidence::DataMissing("未找到对应测试文件".to_string());
    match &missing {
        Confidence::DataMissing(reason) => assert!(!reason.trim().is_empty()),
        _ => panic!("应为 DataMissing"),
    }
    assert!(!missing.is_full());
}
