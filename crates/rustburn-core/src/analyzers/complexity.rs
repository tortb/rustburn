//! ComplexityAnalyzer：复杂度维度。
//!
//! 依赖 [crate::lang::LanguageAdapter]，遍历 AST 计算圈复杂度、
//! if 嵌套深度、函数长度。risk_score = 0.5 × 仓库内百分位 + 0.5 × 绝对阈值。

use std::collections::HashSet;

use serde_json::json;
use tree_sitter::{Node, Tree};

use crate::analyzer::DimensionAnalyzer;
use crate::context::FileContext;
use crate::lang::LanguageAdapter;
use crate::model::{Confidence, DimensionResult};

/// 文件复杂度指标。
#[derive(Debug, Clone)]
pub struct FileComplexity {
    /// 有效代码行数（不含空行和纯注释行）
    pub loc: u32,
    /// 圈复杂度（所有函数平均值，四舍五入到整数）
    pub cyclomatic_complexity: u32,
    /// if 嵌套最大深度
    pub max_if_nesting_depth: u32,
    /// 嵌套 if 比例 = nested_if_count / total_if_count（total==0 时为 0）
    pub nested_if_ratio: f64,
    /// 平均函数长度
    pub avg_function_length: f64,
    /// 最大函数长度
    pub max_function_length: u32,
    /// 语法是否解析不完整
    pub parse_incomplete: bool,
}

/// 函数信息。
#[derive(Debug, Clone)]
struct FunctionInfo {
    start_line: u32,
    end_line: u32,
    complexity: u32,
    if_stats: IfStats,
}

/// if 统计。
#[derive(Debug, Clone, Default)]
struct IfStats {
    total: u32,
    nested: u32,
    chained: u32,
    max_depth: u32,
}

impl IfStats {
    fn merge(&mut self, other: IfStats) {
        self.total += other.total;
        self.nested += other.nested;
        self.chained += other.chained;
        self.max_depth = self.max_depth.max(other.max_depth);
    }

    fn ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.nested as f64 / self.total as f64
        }
    }
}

/// 计算文件复杂度指标。
pub fn compute_metrics(tree: &Tree, source: &str, adapter: &dyn LanguageAdapter) -> FileComplexity {
    let loc = calculate_loc(tree, source);
    let parse_incomplete = tree.root_node().has_error();

    let functions = collect_functions(tree, source, adapter);
    let cyclomatic_complexity = if functions.is_empty() {
        1
    } else {
        let sum: u32 = functions.iter().map(|f| f.complexity).sum();
        (sum as f64 / functions.len() as f64).round() as u32
    };
    let mut if_stats = IfStats::default();
    for f in &functions {
        if_stats.merge(f.if_stats.clone());
    }
    let (avg_function_length, max_function_length) = if functions.is_empty() {
        (0.0, 0)
    } else {
        let max = functions
            .iter()
            .map(|f| f.end_line - f.start_line + 1)
            .max()
            .unwrap_or(0);
        let sum: u32 = functions
            .iter()
            .map(|f| f.end_line - f.start_line + 1)
            .sum();
        (sum as f64 / functions.len() as f64, max)
    };

    FileComplexity {
        loc,
        cyclomatic_complexity,
        max_if_nesting_depth: if_stats.max_depth,
        nested_if_ratio: if_stats.ratio(),
        avg_function_length,
        max_function_length,
        parse_incomplete,
    }
}

/// 基于 AST 计算有效代码行数（跳过注释节点，字符串内注释不会被误判）。
pub fn calculate_loc(tree: &Tree, source: &str) -> u32 {
    let mut code_lines: HashSet<usize> = HashSet::new();
    collect_code_lines(tree.root_node(), source, &mut code_lines);
    code_lines.len() as u32
}

fn collect_code_lines(node: Node<'_>, source: &str, code_lines: &mut HashSet<usize>) {
    // 跳过注释节点（Rust: line_comment/block_comment；JS: comment）
    if node.kind().contains("comment") {
        return;
    }
    if node.child_count() == 0 {
        let text = &source[node.start_byte()..node.end_byte()];
        if !text.trim().is_empty() {
            for row in node.start_position().row..=node.end_position().row {
                code_lines.insert(row);
            }
        }
    } else {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_code_lines(child, source, code_lines);
        }
    }
}

/// 递归提取函数并计算其圈复杂度与 if 统计。
fn collect_functions(
    tree: &Tree,
    source: &str,
    adapter: &dyn LanguageAdapter,
) -> Vec<FunctionInfo> {
    let mut functions = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if adapter.is_function_node(node.kind()) {
            functions.push(FunctionInfo {
                start_line: node.start_position().row as u32,
                end_line: node.end_position().row as u32,
                complexity: 1 + count_decisions(node, source, adapter),
                if_stats: if_stats_in_subtree(node, adapter),
            });
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    functions
}

/// 统计子树内决策点数量。
fn count_decisions(node: Node<'_>, source: &str, adapter: &dyn LanguageAdapter) -> u32 {
    let kind = node.kind();
    let mut count = if let Some(arms) = adapter.count_branches(&node) {
        arms
    } else if adapter.is_branch_node(kind) || is_logical_operator(&node, source) {
        1
    } else {
        0
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_decisions(child, source, adapter);
    }
    count
}

/// 判断节点是否为 && / || 逻辑运算符（两种语言均为 binary_expression）。
fn is_logical_operator(node: &Node<'_>, source: &str) -> bool {
    if node.kind() != "binary_expression" {
        return false;
    }
    node.child_by_field_name("operator")
        .map(|op| {
            let text = &source[op.start_byte()..op.end_byte()];
            text == "&&" || text == "||"
        })
        .unwrap_or(false)
}

/// 统计子树内的 if 嵌套统计。
fn if_stats_in_subtree(node: Node<'_>, adapter: &dyn LanguageAdapter) -> IfStats {
    let mut stats = IfStats::default();
    walk_if_stats(node, adapter, 0, false, &mut stats);
    stats
}

/// 递归遍历 if 节点（链式判定完全依赖 LanguageAdapter，无需源码文本）。
fn walk_if_stats(
    node: Node<'_>,
    adapter: &dyn LanguageAdapter,
    depth: u32,
    is_nested: bool,
    stats: &mut IfStats,
) {
    if adapter.is_if_node(node.kind()) {
        stats.total += 1;
        if is_nested {
            stats.nested += 1;
        }
        // 链式 else-if 不增加嵌套深度；表达式风格的 if-else 按嵌套处理
        let chained = adapter.is_chained_else_if(&node);
        if chained {
            stats.chained += 1;
        }
        let new_depth = if chained { depth } else { depth + 1 };
        stats.max_depth = stats.max_depth.max(new_depth);

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk_if_stats(child, adapter, new_depth, true, stats);
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_if_stats(child, adapter, depth, is_nested, stats);
    }
}

/// 复杂度综合原始值（原 SPEC §68，与 v2 报告透明展示保持一致）。
pub fn complexity_raw_value(m: &FileComplexity) -> f64 {
    (m.cyclomatic_complexity as f64 * 0.4
        + m.max_if_nesting_depth as f64 * 15.0 * 0.4
        + m.avg_function_length * 0.2)
        .clamp(0.0, 100.0)
}

/// 绝对阈值分数（McCabe 圈复杂度 + ESLint max-depth）。
pub fn absolute_complexity_score(m: &FileComplexity) -> f64 {
    let cc_band = match m.cyclomatic_complexity {
        0..=9 => 15.0,
        10..=19 => 50.0,
        20..=49 => 80.0,
        _ => 100.0,
    };
    let depth_band = match m.max_if_nesting_depth {
        0..=4 => 15.0,
        5..=7 => 50.0,
        8..=10 => 80.0,
        _ => 100.0,
    };
    0.7 * cc_band + 0.3 * depth_band
}

/// 仓库内百分位：升序排序、同值取最小 rank、最大值=100。
pub fn repo_percentile(value: f64, pool: &[f64]) -> f64 {
    if pool.is_empty() {
        return 50.0;
    }
    if pool.iter().all(|v| *v == pool[0]) {
        return 50.0;
    }
    let mut sorted = pool.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut r = 0;
    for (i, &v) in sorted.iter().enumerate() {
        if v >= value {
            r = i + 1;
            break;
        }
    }
    if r == 0 {
        r = sorted.len();
    }
    (r as f64 / sorted.len() as f64 * 100.0).clamp(0.0, 100.0)
}

/// ComplexityAnalyzer：依赖 [LanguageAdapter]，不感知语言细节。
pub struct ComplexityAnalyzer;

impl DimensionAnalyzer for ComplexityAnalyzer {
    fn name(&self) -> &'static str {
        "complexity"
    }

    fn analyze(&self, ctx: &FileContext<'_>) -> DimensionResult {
        let Some(tree) = ctx.tree else {
            // 完全无法解析：数据缺失，用仓库均值填充
            let risk = ctx.repo.complexity_risk_mean.unwrap_or(50.0);
            return DimensionResult {
                raw_value: risk,
                risk_score: risk,
                confidence: Confidence::DataMissing("语法解析失败".to_string()),
                detail: json!({ "reason": "tree-sitter 解析返回空结果" }),
            };
        };

        let metrics = compute_metrics(tree, ctx.source, ctx.adapter);
        let raw_value = complexity_raw_value(&metrics);
        let percentile = repo_percentile(raw_value, &ctx.repo.complexity_raw_values);
        let absolute = absolute_complexity_score(&metrics);
        let risk = (0.5 * percentile + 0.5 * absolute).clamp(0.0, 100.0);

        let confidence = if metrics.parse_incomplete {
            Confidence::DataMissing("语法解析不完整".to_string())
        } else {
            Confidence::Full
        };

        DimensionResult {
            raw_value: risk,
            risk_score: risk,
            confidence,
            detail: json!({
                "cyclomatic_complexity": metrics.cyclomatic_complexity,
                "max_if_nesting_depth": metrics.max_if_nesting_depth,
                "nested_if_ratio": metrics.nested_if_ratio,
                "avg_function_length": metrics.avg_function_length,
                "max_function_length": metrics.max_function_length,
                "percentile": percentile,
                "absolute_threshold": absolute,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::RustAdapter;

    fn depth_of(src: &str) -> u32 {
        let adapter = RustAdapter;
        let tree = adapter.parse(src).expect("parse ok");
        compute_metrics(&tree, src, &adapter).max_if_nesting_depth
    }

    /// SPEC v2 §2-A 验收组 1：纯链式 else-if 不增加嵌套深度。
    #[test]
    fn test_pure_chain_else_if_depth_is_one() {
        let src = r#"
fn check(x: i32) {
    if x > 0 {
        println!("a");
    } else if x < 0 {
        println!("b");
    } else if x == 0 {
        println!("c");
    } else {
        println!("d");
    }
}
"#;
        assert_eq!(depth_of(src), 1, "纯链式 else-if 嵌套深度应为 1");
    }

    /// SPEC v2 §2-A 验收组 2：表达式风格 if-else（被赋值包裹）按嵌套处理。
    #[test]
    fn test_expression_style_if_else_is_nested() {
        let src = r#"
fn pick(x: i32) -> i32 {
    let y = if x > 0 {
        1
    } else if x < 0 {
        2
    } else {
        3
    };
    y
}
"#;
        assert_eq!(depth_of(src), 2, "表达式风格 else-if 应按嵌套计深度为 2");
    }

    /// SPEC v2 §2-A 验收组 3：真实嵌套按层数递增。
    #[test]
    fn test_true_nesting_depth() {
        let src = r#"
fn check(x: i32) {
    if x > 0 {
        if x > 10 {
            if x > 100 {
                println!("deep");
            }
        }
    }
}
"#;
        assert_eq!(depth_of(src), 3, "真实三层嵌套深度应为 3");
    }

    /// 混合：外层链式 + 内层真实嵌套，链式不得虚假抬高深度。
    #[test]
    fn test_mixed_chain_and_nesting() {
        let src = r#"
fn check(x: i32, y: i32) {
    if x > 0 {
        if y > 0 {
            println!("a");
        }
    } else if x < 0 {
        println!("b");
    }
}
"#;
        // 外层 if(1) → 内层 if(2)；else-if 不增加
        assert_eq!(depth_of(src), 2);
    }

    /// 圈复杂度统计（match arm / if / 逻辑运算符）。
    #[test]
    fn test_cyclomatic_counts() {
        let adapter = RustAdapter;
        let src = r#"
fn check(x: i32) {
    if x > 0 && x < 10 {
        println!("a");
    }
    match x {
        1 => println!("one"),
        2 => println!("two"),
        _ => println!("other"),
    }
}
"#;
        let tree = adapter.parse(src).unwrap();
        let metrics = compute_metrics(&tree, src, &adapter);
        // if(1) + &&(1) + match 3 arms = 5 → complexity = 1 + 5 = 6
        assert_eq!(metrics.cyclomatic_complexity, 6);
    }

    #[test]
    fn test_repo_percentile_basics() {
        assert_eq!(repo_percentile(10.0, &[]), 50.0);
        let pool = vec![10.0, 20.0, 30.0, 40.0];
        assert!((repo_percentile(40.0, &pool) - 100.0).abs() < 1e-9);
        assert!((repo_percentile(10.0, &pool) - 25.0).abs() < 1e-9);
    }
}
