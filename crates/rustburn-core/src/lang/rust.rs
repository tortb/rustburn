//! Rust 语言适配器。

use crate::lang::{
    in_expression_context, is_alternative_field, parse_source, LanguageAdapter, ParseError,
};
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
        if node.kind() != "if_expression" {
            return false;
        }

        // tree-sitter-rust 用 else_clause 包装 else 分支：
        //   if A { } else if B { }  →  if(A).alternative == else_clause，且 else_clause 直接含 if(B)
        //   if A { } else { if B {} } →  else_clause 内是 block，block 只含 if(B)
        // ① 定位 else 分支容器；② 该容器必须是外层 if 的 alternative 字段
        let else_clause = match node.parent() {
            Some(p) if p.kind() == "else_clause" => p,
            Some(block)
                if block.kind() == "block"
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
        if outer_if.kind() != "if_expression" || !is_alternative_field(&outer_if, &else_clause) {
            return false;
        }

        // ③ 未被赋值/返回等表达式上下文包裹
        !in_expression_context(&outer_if)
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
