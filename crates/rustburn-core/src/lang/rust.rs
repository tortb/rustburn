//! Rust 语言适配器。

use crate::lang::{chained_else_if, parse_source, LanguageAdapter, ParseError};
use crate::model::Language;
use tree_sitter::{Node, Tree};

/// Rust 适配器。
pub struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn parse(&self, source: &str) -> Result<Tree, ParseError> {
        parse_source(Language::Rust, source)
    }

    fn is_branch_node(&self, kind: &str) -> bool {
        matches!(
            kind,
            "if_expression"
                | "for_expression"
                | "while_expression"
                | "loop_expression"
                | "match_expression"
        )
    }

    fn is_function_node(&self, kind: &str) -> bool {
        kind == "function_item"
    }

    fn is_if_node(&self, kind: &str) -> bool {
        kind == "if_expression"
    }

    fn count_branches<'tree>(&self, node: &Node<'tree>) -> Option<u32> {
        if node.kind() != "match_expression" {
            return None;
        }
        Some(count_arms(*node))
    }

    fn is_chained_else_if<'tree>(&self, node: &Node<'tree>) -> bool {
        chained_else_if("if_expression", "block", node)
    }
}

/// 统计 match 表达式内的 arm 数量（含嵌套 match 的 arm）。
fn count_arms(node: Node<'_>) -> u32 {
    let mut count = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "match_arm" => count += 1,
            "match_block" => count += count_arms(child),
            _ => {}
        }
    }
    count
}
