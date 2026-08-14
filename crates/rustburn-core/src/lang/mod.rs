//! 语言适配层（v2 架构）。
//!
//! 每新增一种语言，只需实现 [LanguageAdapter] trait 并在 [adapter_for]
//! 注册表中增加一行；五个 DimensionAnalyzer 不感知任何语言细节。

mod js;
mod mock_lang;
mod rust;

pub use js::JsAdapter;
pub use mock_lang::MockLangAdapter;
pub use rust::RustAdapter;

use crate::model::Language;
use tree_sitter::{Language as TsLanguage, Node, Parser, Tree};

/// AST 解析错误。
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// 该语言没有对应的 tree-sitter 语法
    #[error("不支持的语言: {0}")]
    UnsupportedLanguage(String),
    /// 设置 tree-sitter 语言失败
    #[error("tree-sitter 语言设置失败: {0}")]
    TsLanguage(String),
    /// 解析器产生空结果（语法错误）
    #[error("解析失败（语法错误）: {0}")]
    ParseFailed(String),
}

/// 语言适配器：把所有语言差异收敛在 [parse]、[is_branch_node]、
/// [is_function_node]、[is_chained_else_if] 四个方法内。
///
/// SPEC v2 §1.1：严格照此实现，不允许简化签名。
pub trait LanguageAdapter: Send + Sync {
    /// 适配器对应的语言。
    fn language(&self) -> Language;

    /// 解析源码，返回 tree-sitter 树。
    fn parse(&self, source: &str) -> Result<Tree, ParseError>;

    /// 该节点类型是否计入一个决策点（if/for/while/loop/match 等）。
    fn is_branch_node(&self, kind: &str) -> bool;

    /// 该节点类型是否是函数定义节点。
    fn is_function_node(&self, kind: &str) -> bool;

    /// 判断 `node`（必须是 if 节点）是否为"链式 else-if"。
    ///
    /// SPEC v2 §2.1 禁止事项 2-A：必须同时满足
    /// ① else 分支只包含单个语句 ② 该语句是 if 节点
    /// ③ 该 if 节点没有被赋值/返回等表达式上下文包裹
    /// （否则属于表达式风格的 if-else，应按嵌套处理而非链式）。
    fn is_chained_else_if<'tree>(&self, node: &Node<'tree>) -> bool;

    /// 该节点类型是否是 if 节点（用于 if 嵌套深度/嵌套比例统计）。
    ///
    /// 默认实现返回 false；各语言适配器必须覆盖。
    fn is_if_node(&self, kind: &str) -> bool {
        let _ = kind;
        false
    }

    /// 分支容器（match/switch）节点的决策分支数量；非容器返回 None。
    ///
    /// 默认实现返回 None（按单决策点处理）；Rust/JS 适配器必须覆盖，
    /// 保证 `match` 的每个 arm / `switch` 的每个 case 各计一个决策点。
    fn count_branches<'tree>(&self, node: &Node<'tree>) -> Option<u32> {
        let _ = node;
        None
    }
}

/// 语言注册表：新增语言在这里加一行即可。
pub fn adapter_for(lang: Language) -> Option<Box<dyn LanguageAdapter>> {
    match lang {
        Language::Rust => Some(Box::new(RustAdapter)),
        Language::JavaScript => Some(Box::new(JsAdapter)),
        // 架构解耦验收（SPEC v2 §9）：mock 语言只新增此文件 + 注册表一行。
        Language::Mock => Some(Box::new(MockLangAdapter)),
        Language::Unknown => None,
    }
}

/// 解析 tree-sitter 语言定义。
fn ts_language(lang: Language) -> Option<TsLanguage> {
    match lang {
        Language::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        Language::JavaScript | Language::Mock => Some(tree_sitter_javascript::LANGUAGE.into()),
        Language::Unknown => None,
    }
}

/// 通用解析实现：设置语法并解析，语法错误时仍返回树（调用方自行判断完整性）。
pub(crate) fn parse_source(lang: Language, source: &str) -> Result<Tree, ParseError> {
    let ts = ts_language(lang).ok_or_else(|| ParseError::UnsupportedLanguage(lang.to_string()))?;
    let mut parser = Parser::new();
    parser
        .set_language(&ts)
        .map_err(|e| ParseError::TsLanguage(e.to_string()))?;
    parser
        .parse(source, None)
        .ok_or_else(|| ParseError::ParseFailed("parser 返回空结果".to_string()))
}

/// 通用链式 else-if 判断（rust/js/mock 三个适配器共用，SPEC v2 §2.1 禁止事项 2-A）。
///
/// `if_kind` / `block_kind` 为语言相关节点名（如 `if_expression`/`block`），
/// 其余判定逻辑对三种语言完全一致，故收敛到此公共实现避免重复代码。
pub(crate) fn chained_else_if<'tree>(if_kind: &str, block_kind: &str, node: &Node<'tree>) -> bool {
    if node.kind() != if_kind {
        return false;
    }

    // ① 定位 else 分支容器；② 该容器必须是外层 if 的 alternative 字段
    let else_clause = match node.parent() {
        Some(p) if p.kind() == "else_clause" => p,
        Some(block)
            if block.kind() == block_kind
                && block.named_child_count() == 1
                && block.parent().is_some_and(|p| p.kind() == "else_clause") =>
        {
            block.parent().expect("checked above")
        }
        _ => return false,
    };
    let Some(outer_if) = else_clause.parent() else {
        return false;
    };
    if outer_if.kind() != if_kind || !is_alternative_field(&outer_if, &else_clause) {
        return false;
    }

    // ③ 未被赋值/返回等表达式上下文包裹
    !in_expression_context(&outer_if)
}

/// 判断节点是否为指定 if 节点的 alternative（else）分支。
pub(crate) fn is_alternative_field<'tree>(if_node: &Node<'tree>, child: &Node<'tree>) -> bool {
    if_node
        .child_by_field_name("alternative")
        .is_some_and(|alt| alt == *child)
}

/// 判断节点是否被赋值/返回/调用等"表达式上下文"包裹。
///
/// 从 `node` 向上遍历，遇到语句级边界（函数/源码/普通块）返回 false，
/// 遇到表达式上下文包装节点返回 true。
pub(crate) fn in_expression_context<'tree>(node: &Node<'tree>) -> bool {
    const WRAPPERS: &[&str] = &[
        // Rust
        "let_declaration",
        "assignment_expression",
        "return_expression",
        "call_expression",
        "match_arm",
        // JS
        "variable_declarator",
        "return_statement",
        "arguments",
    ];
    const BOUNDARIES: &[&str] = &[
        "source_file",
        "function_item",
        "function_declaration",
        "function_expression",
        "arrow_function",
        "program",
    ];

    let mut current = *node;
    while let Some(parent) = current.parent() {
        let kind = parent.kind();
        if BOUNDARIES.contains(&kind) {
            return false;
        }
        if WRAPPERS.contains(&kind) {
            return true;
        }
        current = parent;
    }
    false
}
