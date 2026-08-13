//! JavaScript 语言适配器。

use crate::lang::{chained_else_if, parse_source, LanguageAdapter, ParseError};
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
        chained_else_if("if_statement", "statement_block", node)
    }
}
