//! 极简 mock 语言适配器（仅用于 SPEC v2 §9 的架构解耦验收）。
//!
//! 该语言只支持 if / for 两种分支结构（借用 tree-sitter-javascript 语法解析，
//! 但适配器把分支节点收敛为 if/for，函数收敛为 function_declaration），
//! 用于验证"新增语言只需新增此文件 + 注册表一行，分析器与打分逻辑零改动"。

use crate::lang::{chained_else_if, parse_source, LanguageAdapter, ParseError};
use crate::model::Language;
use tree_sitter::{Node, Tree};

/// Mock 语言适配器。
pub struct MockLangAdapter;

impl LanguageAdapter for MockLangAdapter {
    fn language(&self) -> Language {
        Language::Mock
    }

    fn parse(&self, source: &str) -> Result<Tree, ParseError> {
        parse_source(Language::Mock, source)
    }

    /// 只识别 if / for 两种分支结构。
    fn is_branch_node(&self, kind: &str) -> bool {
        matches!(kind, "if_statement" | "for_statement")
    }

    fn is_function_node(&self, kind: &str) -> bool {
        kind == "function_declaration"
    }

    fn is_if_node(&self, kind: &str) -> bool {
        kind == "if_statement"
    }

    fn is_chained_else_if<'tree>(&self, node: &Node<'tree>) -> bool {
        chained_else_if("if_statement", "statement_block", node)
    }
}
