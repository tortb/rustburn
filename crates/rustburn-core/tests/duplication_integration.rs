//! SPEC v2 §3 重复代码维度验收测试。
//!
//! - 测试 A：同一段 10 行逻辑复制 3 份到不同文件、仅改变量名 →
//!   结构哈希必须命中，三个文件的 duplication_risk 显著高于仓库均值；
//! - 测试 B：两段结构相似但字面量类型不同的代码（`x > 0` vs `x == ""`）→ 不得误判为重复；
//! - 测试 C：3-5 行短代码块（哪怕完全相同）→ 低于 6 行阈值，不计入重复。

use std::collections::HashMap;

use rustburn_core::analyzers::complexity::calculate_loc;
use rustburn_core::analyzers::duplication::{
    build_duplication_groups, duplication_risk_from_ranges, DuplicationFileInput,
};
use rustburn_core::lang::adapter_for;
use rustburn_core::model::Language;

/// 10 行左右的函数（> 6 行阈值），三个版本仅变量名不同。
const DUP_FN_A: &str = r#"
fn compute(data: &[u8], limit: usize) -> u32 {
    let mut total = 0u32;
    let mut count = 0usize;
    for item in data {
        total += u32::from(*item);
        count += 1;
        if count >= limit {
            break;
        }
    }
    total
}
"#;

fn renamed(src: &str, from: &str, to: &str) -> String {
    src.replace(from, to)
}

/// 对一组 (path, source) 计算跨文件重复风险，返回 路径 → risk。
fn repo_risks(sources: &[(&str, String)]) -> HashMap<String, f64> {
    let adapter = adapter_for(Language::Rust).expect("rust adapter");

    // 解析全部文件
    let mut parsed: Vec<(String, tree_sitter::Tree, u32)> = Vec::new();
    for (path, src) in sources {
        let tree = adapter.parse(src).expect("parse ok");
        let loc = calculate_loc(&tree, src);
        parsed.push((path.to_string(), tree, loc));
    }

    // 跨文件分组
    let mut inputs: Vec<DuplicationFileInput<'_>> = Vec::new();
    for (path, tree, loc) in &parsed {
        let src = sources
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, s)| s.as_str())
            .expect("find source");
        inputs.push(DuplicationFileInput {
            path,
            tree,
            source: src,
            adapter: adapter.as_ref(),
            loc: *loc,
        });
    }
    let groups = build_duplication_groups(&inputs);

    // 每个文件的风险
    let mut result = HashMap::new();
    for (path, _, loc) in &parsed {
        let ranges = groups.get(path).cloned().unwrap_or_default();
        result.insert(path.clone(), duplication_risk_from_ranges(&ranges, *loc));
    }
    result
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let values: Vec<f64> = values.collect();
    values.iter().sum::<f64>() / values.len() as f64
}

/// 测试 A：复制 + 重命名必须被识别为重复，且风险显著高于仓库均值。
#[test]
fn test_renamed_copy_is_detected_above_repo_mean() {
    let a = DUP_FN_A.to_string();
    let b = renamed(DUP_FN_A, "compute", "calc")
        .replace("data", "payload")
        .replace("item", "element")
        .replace("total", "sum")
        .replace("count", "idx");
    let c = renamed(DUP_FN_A, "compute", "aggregate")
        .replace("data", "input")
        .replace("item", "chunk")
        .replace("total", "acc")
        .replace("count", "n");

    let sources = vec![
        ("a.rs", a),
        ("b.rs", b),
        ("c.rs", c),
        ("u1.rs", "fn u1(x: i32) -> i32 { x * 2 }".to_string()),
        (
            "u2.rs",
            r#"
fn unique_alpha(items: &[i32]) -> i32 {
    let mut best = items.first().copied().unwrap_or(0);
    for v in items {
        if *v > best {
            best = *v;
        }
    }
    best
}
"#
            .to_string(),
        ),
        (
            "u3.rs",
            r#"
fn unique_beta(items: &[String]) -> usize {
    let mut longest = 0usize;
    for s in items {
        if s.len() > longest {
            longest = s.len();
        }
    }
    longest
}
"#
            .to_string(),
        ),
    ];

    let risks = repo_risks(&sources);
    let repo_mean = mean(risks.values().copied());

    for path in ["a.rs", "b.rs", "c.rs"] {
        let r = risks[path];
        assert!(
            r > 60.0,
            "重复文件 {} 的风险应显著高于 0，实际 {:.1}",
            path,
            r
        );
        assert!(
            r > repo_mean,
            "重复文件 {} 的风险({:.1})应高于仓库均值({:.1})",
            path,
            r,
            repo_mean
        );
    }
}

/// 测试 B：结构相似但字面量类型不同（数字 vs 字符串）→ 不得误判为重复。
#[test]
fn test_literal_type_difference_not_duplicate() {
    let src_num = r#"
fn check_a(x: i32) -> bool {
    if x > 0 {
        return true;
    }
    false
}
"#;
    let src_str = r#"
fn check_b(s: String) -> bool {
    if s == "" {
        return true;
    }
    false
}
"#;
    let risks = repo_risks(&[("a.rs", src_num.to_string()), ("b.rs", src_str.to_string())]);
    assert_eq!(risks["a.rs"], 0.0, "数字字面量版本不应被判为重复");
    assert_eq!(risks["b.rs"], 0.0, "字符串字面量版本不应被判为重复");
}

/// 测试 C：3-5 行短代码块即使完全相同也不计入重复（低于 6 行阈值）。
#[test]
fn test_short_blocks_below_threshold_not_counted() {
    // 完全相同的短函数（4 行），在两个文件中
    let short = r#"
fn short_one(x: i32) -> i32 {
    x + 1
}
"#;
    let risks = repo_risks(&[("a.rs", short.to_string()), ("b.rs", short.to_string())]);
    assert_eq!(risks["a.rs"], 0.0, "4 行短块不应计为重复");
    assert_eq!(risks["b.rs"], 0.0, "4 行短块不应计为重复");
}
