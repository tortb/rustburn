//! 代码复杂度分析模块（基于 tree-sitter AST）。
//!
//! 支持 Rust (.rs) 和 JavaScript (.js, .jsx) 的 AST 分析。
//! 计算圈复杂度、if 嵌套深度、函数长度等指标。

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
        collect_code_lines(tree.root_node(), source, lang, &mut code_lines);
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
/// 跳过注释节点（line_comment, block_comment），其他节点标记其所在行。
fn collect_code_lines<'a>(
    node: Node<'a>,
    source: &str,
    lang: Language,
    code_lines: &mut std::collections::HashSet<usize>,
) {
    let kind = node.kind();

    // 跳过注释节点
    let is_comment = match lang {
        Language::Rust => kind == "line_comment" || kind == "block_comment",
        Language::JavaScript => kind == "comment",
        Language::Unknown => false,
    };

    if is_comment {
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
            collect_code_lines(child, source, lang, code_lines);
        }
    }
}

/// 提取所有函数信息
fn extract_functions(tree: &Tree, source: &str, lang: Language) -> Vec<FunctionInfo> {
    let mut functions = Vec::new();
    let root = tree.root_node();

    match lang {
        Language::Rust => extract_rust_functions(root, source, &mut functions),
        Language::JavaScript => extract_js_functions(root, source, &mut functions),
        Language::Unknown => {}
    }

    functions
}

/// 提取 Rust 函数
fn extract_rust_functions(node: Node, source: &str, functions: &mut Vec<FunctionInfo>) {
    let kind = node.kind();

    if kind == "function_item" {
        let name = extract_node_text(node.child_by_field_name("name"), source);
        let complexity = calculate_rust_complexity(node, source);
        let if_stats = calculate_rust_if_stats(node, source, 0, false);

        functions.push(FunctionInfo {
            _name: name,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            complexity,
            if_stats,
        });
    }

    // 递归处理子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_rust_functions(child, source, functions);
    }
}

/// 提取 JavaScript 函数
fn extract_js_functions(node: Node, source: &str, functions: &mut Vec<FunctionInfo>) {
    let kind = node.kind();

    if kind == "function_declaration" || kind == "function_expression" || kind == "arrow_function" {
        let name = extract_node_text(node.child_by_field_name("name"), source);
        let complexity = calculate_js_complexity(node, source);
        let if_stats = calculate_js_if_stats(node, source, 0, false);

        functions.push(FunctionInfo {
            _name: name,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            complexity,
            if_stats,
        });
    }

    // 递归处理子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_js_functions(child, source, functions);
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

/// 计算 Rust 函数的圈复杂度
fn calculate_rust_complexity(node: Node, source: &str) -> u32 {
    let mut complexity = 1; // 基础复杂度

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        complexity += count_rust_decisions(child, source);
    }

    complexity
}

/// 计算 Rust 决策点数量
fn count_rust_decisions(node: Node, source: &str) -> u32 {
    let kind = node.kind();
    let mut count = 0;

    match kind {
        "if_expression" => {
            count += 1;
            // 检查是否是 else if
            if is_else_if_rust(node, source) {
                // else if 不额外计数，因为 if_expression 已经计数
            }
        }
        "match_expression" => {
            // match 的每个 arm 都增加一个 decision
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "match_arm" || child.kind() == "match_block" {
                    count += count_match_arms(child);
                }
            }
        }
        "for_expression" | "while_expression" | "loop_expression" => {
            count += 1;
        }
        "binary_expression" => {
            // 检查当前层级的逻辑运算符
            if let Some(op) = node.child_by_field_name("operator") {
                let op_text = &source[op.start_byte()..op.end_byte()];
                if op_text == "&&" || op_text == "||" {
                    count += 1;
                }
            }
        }
        _ => {}
    }

    // 递归处理子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_rust_decisions(child, source);
    }

    count
}

/// 检查是否是 else if
fn is_else_if_rust(node: Node, source: &str) -> bool {
    // 简化实现：检查前面是否有 else 关键字
    let start = node.start_byte();
    if start > 0 {
        let before = &source[..start];
        let trimmed = before.trim_end();
        return trimmed.ends_with("else");
    }
    false
}

/// 计算 match arm 数量
fn count_match_arms(node: Node) -> u32 {
    let mut count = 0;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "match_arm" {
            count += 1;
        } else if child.kind() == "match_block" {
            count += count_match_arms(child);
        }
    }

    count
}

/// 计算 Rust if 统计
fn calculate_rust_if_stats(node: Node, source: &str, depth: u32, is_nested: bool) -> IfStats {
    let mut stats = IfStats::default();
    let kind = node.kind();

    if kind == "if_expression" {
        stats.total += 1;

        if is_nested {
            stats.nested += 1;
        }

        if is_else_if_rust(node, source) {
            stats.chained += 1;
        }

        let new_depth = if is_else_if_rust(node, source) {
            depth
        } else {
            depth + 1
        };

        stats.max_depth = stats.max_depth.max(new_depth);

        // 递归处理子节点
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_stats = calculate_rust_if_stats(child, source, new_depth, true);
            stats.merge(child_stats);
        }

        return stats;
    }

    // 递归处理子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let child_stats = calculate_rust_if_stats(child, source, depth, is_nested);
        stats.merge(child_stats);
    }

    stats
}

/// 计算 JavaScript 函数的圈复杂度
fn calculate_js_complexity(node: Node, source: &str) -> u32 {
    let mut complexity = 1; // 基础复杂度

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        complexity += count_js_decisions(child, source);
    }

    complexity
}

/// 计算 JavaScript 决策点数量
fn count_js_decisions(node: Node, source: &str) -> u32 {
    let kind = node.kind();
    let mut count = 0;

    match kind {
        "if_statement" => {
            count += 1;
        }
        "switch_statement" => {
            // 每个 case 和 default 都增加一个 decision
            count += count_switch_cases(node);
        }
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
            count += 1;
        }
        "catch_clause" => {
            count += 1;
        }
        "binary_expression" => {
            // 检查当前层级的逻辑运算符
            if let Some(op) = node.child_by_field_name("operator") {
                let op_text = &source[op.start_byte()..op.end_byte()];
                if op_text == "&&" || op_text == "||" {
                    count += 1;
                }
            }
        }
        _ => {}
    }

    // 递归处理子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_js_decisions(child, source);
    }

    count
}

/// 计算 switch case 数量
fn count_switch_cases(node: Node) -> u32 {
    let mut count = 0;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind == "switch_body" {
            count += count_switch_cases(child);
        } else if kind == "switch_case" || kind == "switch_default" {
            count += 1;
        }
    }

    count
}

/// 计算 JavaScript if 统计
fn calculate_js_if_stats(node: Node, source: &str, depth: u32, is_nested: bool) -> IfStats {
    let mut stats = IfStats::default();
    let kind = node.kind();

    if kind == "if_statement" {
        stats.total += 1;

        if is_nested {
            stats.nested += 1;
        }

        if is_else_if_js(node, source) {
            stats.chained += 1;
        }

        let new_depth = if is_else_if_js(node, source) {
            depth
        } else {
            depth + 1
        };

        stats.max_depth = stats.max_depth.max(new_depth);

        // 递归处理子节点
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_stats = calculate_js_if_stats(child, source, new_depth, true);
            stats.merge(child_stats);
        }

        return stats;
    }

    // 递归处理子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let child_stats = calculate_js_if_stats(child, source, depth, is_nested);
        stats.merge(child_stats);
    }

    stats
}

/// 检查是否是 else if
fn is_else_if_js(node: Node, source: &str) -> bool {
    let start = node.start_byte();
    if start > 0 {
        let before = &source[..start];
        let trimmed = before.trim_end();
        return trimmed.ends_with("else");
    }
    false
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
