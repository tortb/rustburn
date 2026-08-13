//! DuplicationAnalyzer：重复代码维度。
//!
//! 算法（SPEC v2 §3）：
//! 1. 对每个函数/代码块生成"结构哈希"：遍历子树，所有标识符替换为 IDENT，
//!    保留语法结构与字面量类型（数字/字符串/布尔），不保留具体值；
//! 2. 只对超过 6 行的代码块参与判重；
//! 3. 哈希相同的块归为一组（**跨文件统一分组**），统计每个文件参与"重复组"
//!    的行数占比；`duplication_risk_value = min(100, 重复行数占比 × 150)`。
//!
//! 禁止使用文本级（逐行字符串比较）判重——重命名变量即可绕过；
//! 禁止丢弃字面量类型信息——`if (x > 0)` 与 `if (x == "")` 不得误判为重复。

use std::collections::{HashMap, HashSet};

use serde_json::json;
use tree_sitter::{Node, Tree};

use crate::analyzer::DimensionAnalyzer;
use crate::context::FileContext;
use crate::lang::LanguageAdapter;
use crate::model::{Confidence, DimensionResult, Language};

/// 参与判重的代码块最小行数（含函数签名行）。
const MIN_BLOCK_LINES: u32 = 6;

/// 仓库级判重输入：一个文件（语法树已解析）。
pub struct DuplicationFileInput<'a> {
    /// 文件相对路径
    pub path: &'a str,
    /// 语法树
    pub tree: &'a Tree,
    /// 源码
    pub source: &'a str,
    /// 语言适配器
    pub adapter: &'a dyn LanguageAdapter,
    /// 有效代码行数
    pub loc: u32,
}

/// 结构哈希项。
fn classify_leaf(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return "<empty>".to_string();
    }
    // 字符串字面量（含原生字符串前缀 r"..." / r#"..."#）
    if t.starts_with('"')
        || t.starts_with('\'')
        || t.starts_with('`')
        || (t.starts_with('r') && (t[1..].starts_with('"') || t[1..].starts_with('#')))
    {
        return "STR".to_string();
    }
    // 数字字面量
    if t.parse::<f64>().is_ok() || t.starts_with(|c: char| c.is_ascii_digit()) {
        return "NUM".to_string();
    }
    // 布尔/空值字面量
    if matches!(t, "true" | "false" | "null" | "nil" | "None") {
        return "BOOL".to_string();
    }
    // 标识符（变量名/函数名等）
    if t.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$' || c == '@')
    {
        return "IDENT".to_string();
    }
    // 操作符/关键字等匿名节点：保留具体值（语言内稳定）
    t.to_string()
}

/// 对节点子树生成结构哈希。
fn structural_hash(node: &Node<'_>, source: &str) -> u64 {
    let mut parts: Vec<String> = Vec::new();
    collect_hash_parts(*node, source, &mut parts);
    // FNV-1a 简化哈希（无需引入第三方依赖）
    let mut hash: u64 = 0xcbf29ce484222325;
    for part in parts {
        for b in part.bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn collect_hash_parts(node: Node<'_>, source: &str, parts: &mut Vec<String>) {
    let kind = node.kind();
    if kind.contains("comment") {
        return;
    }
    if node.child_count() == 0 {
        if node.is_named() {
            parts.push(classify_leaf(&source[node.start_byte()..node.end_byte()]));
        } else {
            parts.push(kind.to_string());
        }
        return;
    }
    parts.push(kind.to_string());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_hash_parts(child, source, parts);
    }
}

/// 单个函数的哈希与行区间。
#[derive(Debug, Clone)]
struct BlockHash {
    hash: u64,
    path: String,
    start_line: u32,
    end_line: u32,
}

/// 对仓库全部文件做跨文件结构哈希分组。
///
/// 返回：文件路径 → 参与"重复组"的行区间列表（组内 >= 2 个成员）。
pub fn build_duplication_groups(
    files: &[DuplicationFileInput<'_>],
) -> HashMap<String, Vec<(u32, u32)>> {
    // 1. 收集所有文件的函数块（> 6 行）
    let mut blocks: Vec<BlockHash> = Vec::new();
    for file in files {
        blocks.extend(collect_block_hashes(
            file.tree,
            file.path,
            file.source,
            file.adapter,
        ));
    }

    // 2. 按哈希跨文件分组
    let mut groups: HashMap<u64, Vec<&BlockHash>> = HashMap::new();
    for b in &blocks {
        groups.entry(b.hash).or_default().push(b);
    }

    // 3. 组内 >= 2 成员 → 标记该文件的这些行区间为重复
    let mut result: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    for members in groups.values() {
        if members.len() >= 2 {
            for m in members {
                result
                    .entry(m.path.clone())
                    .or_default()
                    .push((m.start_line, m.end_line));
            }
        }
    }
    result
}

/// 计算一个文件内所有函数的结构哈希块（> 6 行才参与）。
fn collect_block_hashes(
    tree: &Tree,
    path: &str,
    source: &str,
    adapter: &dyn LanguageAdapter,
) -> Vec<BlockHash> {
    let mut blocks = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if adapter.is_function_node(node.kind()) {
            let start = node.start_position().row as u32;
            let end = node.end_position().row as u32;
            if end - start + 1 > MIN_BLOCK_LINES {
                blocks.push(BlockHash {
                    hash: structural_hash(&node, source),
                    path: path.to_string(),
                    start_line: start,
                    end_line: end,
                });
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    blocks
}

/// 由重复行区间计算风险分（0-100）。
pub fn duplication_risk_from_ranges(ranges: &[(u32, u32)], loc: u32) -> f64 {
    if ranges.is_empty() || loc == 0 {
        return 0.0;
    }
    let mut duplicated_lines: HashSet<u32> = HashSet::new();
    for (start, end) in ranges {
        for line in *start..=*end {
            duplicated_lines.insert(line);
        }
    }
    let ratio = duplicated_lines.len() as f64 / loc as f64;
    (ratio * 150.0).min(100.0)
}

/// DuplicationAnalyzer：依赖 [LanguageAdapter]，不感知语言细节。
pub struct DuplicationAnalyzer;

impl DimensionAnalyzer for DuplicationAnalyzer {
    fn name(&self) -> &'static str {
        "duplication"
    }

    fn analyze(&self, ctx: &FileContext<'_>) -> DimensionResult {
        // 语言暂不支持重复检测 → NotApplicable（由 scoring 重新分摊权重）
        if ctx.language == Language::Unknown {
            return DimensionResult {
                raw_value: 0.0,
                risk_score: 0.0,
                confidence: Confidence::NotApplicable,
                detail: json!({ "reason": "语言不支持重复检测" }),
            };
        }

        // 语法完全无法解析：数据缺失，用仓库均值填充
        if ctx.tree.is_none() {
            let risk = ctx.repo.duplication_risk_mean.unwrap_or(50.0);
            return DimensionResult {
                raw_value: risk,
                risk_score: risk,
                confidence: Confidence::DataMissing("语法解析失败".to_string()),
                detail: json!({ "reason": "tree-sitter 解析返回空结果" }),
            };
        }

        let ranges = ctx
            .repo
            .duplication_line_ranges
            .get(ctx.path)
            .cloned()
            .unwrap_or_default();
        let risk = duplication_risk_from_ranges(&ranges, ctx.loc);

        let confidence = if ctx.parse_incomplete {
            Confidence::DataMissing("语法解析不完整".to_string())
        } else {
            Confidence::Full
        };

        DimensionResult {
            raw_value: risk,
            risk_score: risk,
            confidence,
            detail: json!({
                "duplicated_ranges": ranges.len(),
                "duplicated_line_ratio": risk / 150.0,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_leaf_types() {
        assert_eq!(classify_leaf("foo"), "IDENT");
        assert_eq!(classify_leaf("_tmp"), "IDENT");
        assert_eq!(classify_leaf("\"str\""), "STR");
        assert_eq!(classify_leaf("'c'"), "STR");
        assert_eq!(classify_leaf("42"), "NUM");
        assert_eq!(classify_leaf("3.14"), "NUM");
        assert_eq!(classify_leaf("true"), "BOOL");
        assert_eq!(classify_leaf("false"), "BOOL");
        // 操作符保留具体值
        assert_eq!(classify_leaf(">"), ">");
        assert_eq!(classify_leaf("+"), "+");
    }

    fn first_fn<'a>(tree: &'a Tree, source: &str) -> Node<'a> {
        let _ = source;
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "function_item" {
                return n;
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        unreachable!()
    }

    #[test]
    fn test_structural_hash_renamed_variables_same() {
        use crate::lang::adapter_for;
        use crate::model::Language;
        let adapter = adapter_for(Language::Rust).unwrap();
        let src_a = r#"
fn process(data: &[u8]) -> u32 {
    let mut total = 0;
    for item in data {
        total += item as u32;
    }
    total
}
"#;
        let src_b = src_a
            .replace("total", "sum")
            .replace("item", "element")
            .replace("data", "payload");
        let tree_a = adapter.parse(src_a).unwrap();
        let tree_b = adapter.parse(&src_b).unwrap();
        let ha = structural_hash(&first_fn(&tree_a, src_a), src_a);
        let hb = structural_hash(&first_fn(&tree_b, &src_b), &src_b);
        assert_eq!(ha, hb, "仅重命名变量不应改变结构哈希");
    }

    #[test]
    fn test_structural_hash_literal_type_differs() {
        use crate::lang::adapter_for;
        use crate::model::Language;
        let adapter = adapter_for(Language::Rust).unwrap();
        let src_a = r#"
fn check(x: i32) -> bool {
    if x > 0 {
        return true;
    }
    false
}
"#;
        let src_b = r#"
fn check(x: String) -> bool {
    if x == "" {
        return true;
    }
    false
}
"#;
        let tree_a = adapter.parse(src_a).unwrap();
        let tree_b = adapter.parse(src_b).unwrap();
        let ha = structural_hash(&first_fn(&tree_a, src_a), src_a);
        let hb = structural_hash(&first_fn(&tree_b, src_b), src_b);
        assert_ne!(ha, hb, "数字与字符串字面量类型不同，哈希必须不同");
    }

    #[test]
    fn test_risk_from_ranges() {
        assert_eq!(duplication_risk_from_ranges(&[], 100), 0.0);
        assert_eq!(duplication_risk_from_ranges(&[], 0), 0.0);
        // 10 行重复 / 100 行 → min(100, 0.1*150) = 15
        assert_eq!(duplication_risk_from_ranges(&[(0, 9)], 100), 15.0);
        // 全部重复 → 100
        assert_eq!(duplication_risk_from_ranges(&[(0, 99)], 100), 100.0);
    }
}
