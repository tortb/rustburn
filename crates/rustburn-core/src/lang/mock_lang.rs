//! 极简 mock 语言适配器（仅用于 SPEC v2 §9 的架构解耦验收）。
//!
//! 该语言只支持 if / for 两种分支结构（借用 tree-sitter-javascript 语法解析，
//! 但适配器把分支节点收敛为 if/for，函数收敛为 function_declaration），
//! 用于验证"新增语言只需新增此文件 + 注册表一行，分析器与打分逻辑零改动"。

use crate::lang::{
    in_expression_context, is_alternative_field, parse_source, LanguageAdapter, ParseError,
};
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
        if node.kind() != "if_statement" {
            return false;
        }
        let else_clause = match node.parent() {
            Some(p) if p.kind() == "else_clause" => p,
            Some(block)
                if block.kind() == "statement_block"
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
        if outer_if.kind() != "if_statement" || !is_alternative_field(&outer_if, &else_clause) {
            return false;
        }
        !in_expression_context(&outer_if)
    }
}
