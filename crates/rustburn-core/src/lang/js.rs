//! JavaScript 语言适配器。

use crate::lang::{
    in_expression_context, is_alternative_field, parse_source, LanguageAdapter, ParseError,
};
use crate::model::Language;
use tree_sitter::{Node, Tree};

/// JavaScript 适配器。
pub struct JsAdapter;

impl LanguageAdapter for JsAdapter {
    fn language(&self) -> Language {
        Language::JavaScript
    }

    fn parse(&self, source: &str) -> Result<Tree, ParseError> {
        parse_source(Language::JavaScript, source)
    }

    fn is_branch_node(&self, kind: &str) -> bool {
        matches!(
            kind,
            "if_statement"
                | "for_statement"
                | "for_in_statement"
                | "for_of_statement"
                | "while_statement"
                | "do_statement"
                | "switch_statement"
                | "catch_clause"
        )
    }

    fn is_function_node(&self, kind: &str) -> bool {
        matches!(
            kind,
            "function_declaration" | "function_expression" | "arrow_function"
        )
    }

    fn is_if_node(&self, kind: &str) -> bool {
        kind == "if_statement"
    }

    fn count_branches<'tree>(&self, node: &Node<'tree>) -> Option<u32> {
        if node.kind() != "switch_statement" {
            return None;
        }
        let mut count = 0;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "switch_case" | "switch_default" => count += 1,
                _ => {}
            }
        }
        Some(count)
    }

    fn is_chained_else_if<'tree>(&self, node: &Node<'tree>) -> bool {
        if node.kind() != "if_statement" {
            return false;
        }

        // tree-sitter-javascript 同样用 else_clause 包装 else 分支。
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

        // ③ 未被赋值/返回等表达式上下文包裹
        !in_expression_context(&outer_if)
    }
}
