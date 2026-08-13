//! 代码复杂度分析模块（基于 tree-sitter AST）。
//!
//! 支持 Rust (.rs) 和 JavaScript (.js, .jsx) 的 AST 分析。
//! 计算圈复杂度、if 嵌套深度、函数长度等指标。
//!
//! 遍历与统计逻辑均为语言无关的通用框架，语言差异仅体现在
//! [AstSpec] 的节点类型映射表上。

use tree_sitter::{Language as TsLanguage, Node, Parser, Tree};

use crate::model::Language;

/// 文件复杂度指标
#[derive(Debug, Clone)]
pub struct FileComplexity {
    /// 有效代码行数（不含空行和纯注释行）
    pub loc: u32,
    /// 圈复杂度（所有函数平均值，四舍五入到整数）
    pub cyclomatic_complexity: u32,
    /// if 嵌套最大深度
    pub max_if_nesting_depth: u32,
    /// 嵌套 if 比例 = nested_if_count / total_if_count（total_if_count==0 时为 0）
    pub nested_if_ratio: f64,
    /// 平均函数长度
    pub avg_function_length: f64,
    /// 最大函数长度
    pub max_function_length: u32,
    /// 是否语法解析不完整
    pub parse_incomplete: bool,
}

/// 函数信息
#[derive(Debug, Clone)]
struct FunctionInfo {
    /// 函数名称（可选）
    _name: Option<String>,
    /// 起始行号
    start_line: u32,
    /// 结束行号
    end_line: u32,
    /// 圈复杂度
    complexity: u32,
    /// if 统计
    if_stats: IfStats,
}

/// if 统计信息
#[derive(Debug, Clone, Default)]
struct IfStats {
    /// 总 if 数量
    total: u32,
    /// 嵌套 if 数量（在另一个 if 内部的 if）
    nested: u32,
    /// 链式 if 数量（else if）
    chained: u32,
    /// 最大嵌套深度
    max_depth: u32,
}

/// 语言无关的 AST 遍历框架。
///
/// 每种语言只需提供节点类型映射表，所有遍历与统计逻辑（LOC、函数提取、
/// 决策点计数、if 统计）全部复用。
trait AstSpec {
    /// 函数定义节点类型
    const FUNCTIONS: &'static [&'static str];
    /// 注释节点类型
    const COMMENTS: &'static [&'static str];
    /// if 节点类型
    const IF: &'static str;
    /// 计 1 个决策点的节点类型
    const DECISION_POINTS: &'static [&'static str];
    /// 分支容器节点类型（match/switch），其决策点数按分支子节点统计
    const BRANCH_CONTAINER: &'static str;
    /// 分支子节点类型
    const BRANCHES: &'static [&'static str];
    /// 分支容器内可嵌套的子容器类型
    const NESTED_CONTAINER: &'static str;
}

/// Rust 节点类型映射表
struct RustAst;

impl AstSpec for RustAst {
    const FUNCTIONS: &'static [&'static str] = &["function_item"];
    const COMMENTS: &'static [&'static str] = &["line_comment", "block_comment"];
    const IF: &'static str = "if_expression";
    const DECISION_POINTS: &'static [&'static str] = &[
        "if_expression",
        "for_expression",
        "while_expression",
        "loop_expression",
    ];
    const BRANCH_CONTAINER: &'static str = "match_expression";
    const BRANCHES: &'static [&'static str] = &["match_arm"];
    const NESTED_CONTAINER: &'static str = "match_block";
}

/// JavaScript 节点类型映射表
struct JsAst;

impl AstSpec for JsAst {
    const FUNCTIONS: &'static [&'static str] = &[
        "function_declaration",
        "function_expression",
        "arrow_function",
    ];
    const COMMENTS: &'static [&'static str] = &["comment"];
    const IF: &'static str = "if_statement";
    const DECISION_POINTS: &'static [&'static str] = &[
        "if_statement",
        "for_statement",
        "for_in_statement",
        "while_statement",
        "do_statement",
        "catch_clause",
    ];
    const BRANCH_CONTAINER: &'static str = "switch_statement";
    const BRANCHES: &'static [&'static str] = &["switch_case", "switch_default"];
    const NESTED_CONTAINER: &'static str = "switch_body";
}

/// 获取 tree-sitter 语言定义
fn get_ts_language(lang: Language) -> Option<TsLanguage> {
    match lang {
        Language::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        Language::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
        Language::Unknown => None,
    }
}

/// 检测文件语言
pub fn detect_language(path: &str) -> Language {
    let path_lower = path.to_lowercase();
    if path_lower.ends_with(".rs") {
        Language::Rust
    } else if path_lower.ends_with(".js") || path_lower.ends_with(".jsx") {
        Language::JavaScript
    } else {
        Language::Unknown
    }
}

/// 分析文件复杂度
pub fn analyze_complexity(source: &str, lang: Language) -> Result<FileComplexity, String> {
    let ts_lang = get_ts_language(lang).ok_or_else(|| "不支持的语言".to_string())?;

    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .map_err(|e| format!("设置语言失败: {}", e))?;

    let tree = parser.parse(source, None);

    // 基于 AST 计算 LOC（需要 tree 引用）
    let loc = calculate_loc(source, lang, tree.as_ref());

    match tree {
        Some(tree) => {
            // 检查是否有语法错误（ERROR 节点）
            let parse_incomplete = has_syntax_errors(&tree);

            let functions = extract_functions(&tree, source, lang);
            let complexity = calculate_complexity(&functions);
            let if_stats = aggregate_if_stats(&functions);
            let (avg_func_len, max_func_len) = calculate_function_lengths(&functions);

            Ok(FileComplexity {
                loc,
                cyclomatic_complexity: complexity,
                max_if_nesting_depth: if_stats.max_depth,
                nested_if_ratio: if_stats.ratio(),
                avg_function_length: avg_func_len,
                max_function_length: max_func_len,
                parse_incomplete,
            })
        }
        None => {
            // 语法错误：返回最小指标（loc 已在上方计算）
            Ok(FileComplexity {
                loc,
                cyclomatic_complexity: 1,
                max_if_nesting_depth: 0,
                nested_if_ratio: 0.0,
                avg_function_length: 0.0,
                max_function_length: 0,
                parse_incomplete: true,
            })
        }
    }
}

/// 检查 AST 中是否存在语法错误（ERROR 节点）
fn has_syntax_errors(tree: &Tree) -> bool {
    let root = tree.root_node();
    root.has_error()
}

/// 计算有效代码行数（LOC）
///
/// 基于 tree-sitter AST：遍历所有叶子节点，标记包含非注释 token 的行。
/// 空行和纯注释行不计入。字符串内的注释字符不会被误判。
fn calculate_loc(source: &str, lang: Language, tree: Option<&Tree>) -> u32 {
    let total_lines = source.lines().count();
    if total_lines == 0 {
        return 0;
    }

    // 使用 HashSet 记录包含代码 token 的行号
    let mut code_lines: std::collections::HashSet<usize> = std::collections::HashSet::new();

    if let Some(tree) = tree {
        // 遍历 AST 所有节点，收集非注释的叶子节点所在的行
        match lang {
            Language::Rust => {
                collect_code_lines::<RustAst>(tree.root_node(), source, &mut code_lines)
            }
            Language::JavaScript => {
                collect_code_lines::<JsAst>(tree.root_node(), source, &mut code_lines)
            }
            Language::Unknown => {}
        }
    } else {
        // 解析失败时回退到简单计数（非空行）
        for (i, line) in source.lines().enumerate() {
            if !line.trim().is_empty() {
                code_lines.insert(i);
            }
        }
    }

    code_lines.len() as u32
}

/// 递归遍历 AST，收集包含代码 token 的行号。
///
/// 跳过注释节点，其他节点标记其所在行。
fn collect_code_lines<T: AstSpec>(
    node: Node<'_>,
    source: &str,
    code_lines: &mut std::collections::HashSet<usize>,
) {
    // 跳过注释节点
    if T::COMMENTS.contains(&node.kind()) {
        return;
    }

    // 如果是叶子节点（无子节点），标记其所有行
    if node.child_count() == 0 {
        let start_row = node.start_position().row;
        let end_row = node.end_position().row;

        // 检查该 token 是否为非空白内容
        let text = &source[node.start_byte()..node.end_byte()];
        if !text.trim().is_empty() {
            for row in start_row..=end_row {
                code_lines.insert(row);
            }
        }
    } else {
        // 递归处理子节点
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_code_lines::<T>(child, source, code_lines);
        }
    }
}

/// 提取所有函数信息
fn extract_functions(tree: &Tree, source: &str, lang: Language) -> Vec<FunctionInfo> {
    let mut functions = Vec::new();

    match lang {
        Language::Rust => collect_functions::<RustAst>(tree.root_node(), source, &mut functions),
        Language::JavaScript => {
            collect_functions::<JsAst>(tree.root_node(), source, &mut functions)
        }
        Language::Unknown => {}
    }

    functions
}

/// 递归遍历 AST，提取函数信息
fn collect_functions<T: AstSpec>(node: Node<'_>, source: &str, functions: &mut Vec<FunctionInfo>) {
    if T::FUNCTIONS.contains(&node.kind()) {
        functions.push(FunctionInfo {
            _name: extract_node_text(node.child_by_field_name("name"), source),
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            complexity: calculate_function_complexity::<T>(node, source),
            if_stats: calculate_if_stats::<T>(node, source, 0, false),
        });
    }

    // 递归处理子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_functions::<T>(child, source, functions);
    }
}

/// 提取节点文本
fn extract_node_text(node: Option<Node>, source: &str) -> Option<String> {
    node.map(|n| {
        let start = n.start_byte();
        let end = n.end_byte();
        source[start..end].to_string()
    })
}

/// 计算函数圈复杂度（基础 1 + 所有子节点的决策点数）。
fn calculate_function_complexity<T: AstSpec>(node: Node<'_>, source: &str) -> u32 {
    let mut cursor = node.walk();
    1 + node
        .children(&mut cursor)
        .map(|child| count_decisions::<T>(child, source))
        .sum::<u32>()
}

/// 计算节点及其子树的决策点数量。
fn count_decisions<T: AstSpec>(node: Node<'_>, source: &str) -> u32 {
    let kind = node.kind();
    let mut count = match kind {
        k if T::DECISION_POINTS.contains(&k) => 1,
        k if k == T::BRANCH_CONTAINER => count_branches::<T>(node),
        "binary_expression" => {
            // 检查当前层级的逻辑运算符
            if let Some(op) = node.child_by_field_name("operator") {
                let op_text = &source[op.start_byte()..op.end_byte()];
                if op_text == "&&" || op_text == "||" {
                    1
                } else {
                    0
                }
            } else {
                0
            }
        }
        _ => 0,
    };

    // 递归处理子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_decisions::<T>(child, source);
    }

    count
}

/// 统计分支容器的分支数量（match arm / switch case）。
fn count_branches<T: AstSpec>(node: Node<'_>) -> u32 {
    let mut count = 0;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if T::BRANCHES.contains(&kind) {
            count += 1;
        } else if kind == T::NESTED_CONTAINER {
            count += count_branches::<T>(child);
        }
    }

    count
}

/// 计算子树内的 if 统计。
///
/// 设计要点：链式 if（else if）不增加嵌套深度，嵌套 if 增加深度。
fn calculate_if_stats<T: AstSpec>(
    node: Node<'_>,
    source: &str,
    depth: u32,
    is_nested: bool,
) -> IfStats {
    let mut stats = IfStats::default();

    if node.kind() == T::IF {
        stats.total += 1;

        if is_nested {
            stats.nested += 1;
        }

        let is_else_if = is_else_if(node, source);
        if is_else_if {
            stats.chained += 1;
        }

        // 链式 if（else if）不增加嵌套深度，嵌套 if 深度 +1
        let new_depth = if is_else_if { depth } else { depth + 1 };
        stats.max_depth = stats.max_depth.max(new_depth);

        // 递归处理子节点
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stats.merge(calculate_if_stats::<T>(child, source, new_depth, true));
        }

        return stats;
    }

    // 递归处理子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        stats.merge(calculate_if_stats::<T>(child, source, depth, is_nested));
    }

    stats
}

/// 检查是否是 else if（节点前面紧跟 else 关键字）。
fn is_else_if(node: Node<'_>, source: &str) -> bool {
    let start = node.start_byte();
    start > 0 && source[..start].trim_end().ends_with("else")
}

impl IfStats {
    /// 合并另一个 IfStats
    fn merge(&mut self, other: IfStats) {
        self.total += other.total;
        self.nested += other.nested;
        self.chained += other.chained;
        self.max_depth = self.max_depth.max(other.max_depth);
    }

    /// 计算嵌套比例
    fn ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.nested as f64 / self.total as f64
        }
    }
}

/// 计算所有函数的平均圈复杂度
fn calculate_complexity(functions: &[FunctionInfo]) -> u32 {
    if functions.is_empty() {
        return 1;
    }

    let sum: u32 = functions.iter().map(|f| f.complexity).sum();
    let avg = sum as f64 / functions.len() as f64;
    avg.round() as u32
}

/// 聚合所有函数的 if 统计
fn aggregate_if_stats(functions: &[FunctionInfo]) -> IfStats {
    let mut stats = IfStats::default();
    for func in functions {
        stats.merge(func.if_stats.clone());
    }
    stats
}

/// 计算函数长度统计
fn calculate_function_lengths(functions: &[FunctionInfo]) -> (f64, u32) {
    if functions.is_empty() {
        return (0.0, 0);
    }

    let lengths: Vec<u32> = functions
        .iter()
        .map(|f| {
            let len = f.end_line - f.start_line + 1;
            len.max(1) // 最小长度为 1
        })
        .collect();

    let max = lengths.iter().copied().max().unwrap_or(0);
    let sum: u32 = lengths.iter().sum();
    let avg = sum as f64 / lengths.len() as f64;

    (avg, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language("main.rs"), Language::Rust);
        assert_eq!(detect_language("app.js"), Language::JavaScript);
        assert_eq!(detect_language("component.jsx"), Language::JavaScript);
        assert_eq!(detect_language("unknown.txt"), Language::Unknown);
    }

    #[test]
    fn test_empty_file() {
        let result = analyze_complexity("", Language::Rust).unwrap();
        assert_eq!(result.loc, 0);
        assert_eq!(result.cyclomatic_complexity, 1);
    }

    #[test]
    fn test_comment_only() {
        let source = "// 这是注释\n/* 块注释 */\n";
        let result = analyze_complexity(source, Language::Rust).unwrap();
        assert_eq!(result.loc, 0);
    }

    #[test]
    fn test_simple_function() {
        let source = r#"
fn main() {
    println!("Hello");
}
"#;
        let result = analyze_complexity(source, Language::Rust).unwrap();
        assert!(result.loc > 0);
        assert_eq!(result.cyclomatic_complexity, 1);
    }

    #[test]
    fn test_if_else() {
        let source = r#"
fn check(x: i32) {
    if x > 0 {
        println!("positive");
    } else {
        println!("non-positive");
    }
}
"#;
        let result = analyze_complexity(source, Language::Rust).unwrap();
        assert_eq!(result.cyclomatic_complexity, 2);
    }

    #[test]
    fn test_nested_if() {
        let source = r#"
fn check(x: i32, y: i32) {
    if x > 0 {
        if y > 0 {
            println!("both positive");
        }
    }
}
"#;
        let result = analyze_complexity(source, Language::Rust).unwrap();
        assert_eq!(result.cyclomatic_complexity, 3);
        assert_eq!(result.max_if_nesting_depth, 2);
    }

    #[test]
    fn test_else_if_chain() {
        let source = r#"
fn check(x: i32) {
    if x > 0 {
        println!("positive");
    } else if x < 0 {
        println!("negative");
    } else if x == 0 {
        println!("zero");
    } else {
        println!("other");
    }
}
"#;
        let result = analyze_complexity(source, Language::Rust).unwrap();
        // 4 个 if/else if 分支
        assert_eq!(result.cyclomatic_complexity, 4);
        // else if 不增加嵌套深度
        assert_eq!(result.max_if_nesting_depth, 1);
    }

    #[test]
    fn test_logical_operators() {
        let source = r#"
fn check(a: bool, b: bool, c: bool) {
    if a && b || c {
        println!("complex condition");
    }
}
"#;
        let result = analyze_complexity(source, Language::Rust).unwrap();
        // base(1) + if(1) + &&(1) + ||(1) = 4
        assert_eq!(result.cyclomatic_complexity, 4);
    }

    #[test]
    fn test_match_expression() {
        let source = r#"
fn check(x: i32) {
    match x {
        1 => println!("one"),
        2 => println!("two"),
        _ => println!("other"),
    }
}
"#;
        let result = analyze_complexity(source, Language::Rust).unwrap();
        // 3 个 match arm
        assert_eq!(result.cyclomatic_complexity, 4);
    }

    #[test]
    fn test_js_switch() {
        let source = r#"
function check(x) {
    switch (x) {
        case 1:
            console.log("one");
            break;
        case 2:
            console.log("two");
            break;
        default:
            console.log("other");
    }
}
"#;
        let result = analyze_complexity(source, Language::JavaScript).unwrap();
        // 2 case + 1 default = 3
        assert_eq!(result.cyclomatic_complexity, 4);
    }

    #[test]
    fn test_js_catch() {
        let source = r#"
function check() {
    try {
        riskyOperation();
    } catch (e) {
        console.log(e);
    }
}
"#;
        let result = analyze_complexity(source, Language::JavaScript).unwrap();
        // catch 增加 1
        assert_eq!(result.cyclomatic_complexity, 2);
    }

    #[test]
    fn test_js_arrow_function() {
        let source = r#"
const check = (x) => {
    if (x > 0) {
        return x;
    }
    return 0;
};
"#;
        let result = analyze_complexity(source, Language::JavaScript).unwrap();
        assert_eq!(result.cyclomatic_complexity, 2);
    }

    #[test]
    fn test_syntax_error() {
        let source = r#"
fn broken( {
    // 语法错误
}
"#;
        let result = analyze_complexity(source, Language::Rust).unwrap();
        assert!(result.parse_incomplete);
    }

    #[test]
    fn test_function_length() {
        let source = r#"
fn short() {
    println!("short");
}

fn long() {
    println!("line 1");
    println!("line 2");
    println!("line 3");
    println!("line 4");
    println!("line 5");
}
"#;
        let result = analyze_complexity(source, Language::Rust).unwrap();
        assert!(result.max_function_length >= 3);
        assert!(result.avg_function_length > 0.0);
    }
}
