//! Go 语言适配器。
//!
//! 只依赖 [crate::lang::LanguageAdapter] 抽象：分支/函数节点映射、
//! 链式 else-if 判定复用 [crate::lang::chained_else_if] 公共实现。
//!
//! 另外提供两段 Go 特有的辅助逻辑（放在本文件、独立于 analyzer）：
//! - [go_assertion_count]：Go 测试断言计数（testify require/assert、
//!   `t.Errorf`/`t.Fatalf` 等，与 rust/js 的 assert/expect 模式不同）；
//! - [go_cover_to_cobertura]：`go tool cover` 输出 → Cobertura XML
//!   （Go 原生覆盖率格式不是 lcov/cobertura，必须先转换才能复用
//!   已有的覆盖率解析器，否则 Go 项目覆盖率维度会全部变成数据缺失）。

use std::collections::HashMap;

use crate::lang::{chained_else_if, parse_source, LanguageAdapter, ParseError};
use crate::model::Language;
use tree_sitter::{Node, Tree};

/// Go 适配器。
pub struct GoAdapter;

impl LanguageAdapter for GoAdapter {
    fn language(&self) -> Language {
        Language::Go
    }

    fn parse(&self, source: &str) -> Result<Tree, ParseError> {
        parse_source(Language::Go, source)
    }

    fn is_branch_node(&self, kind: &str) -> bool {
        matches!(
            kind,
            "if_statement"
                | "for_statement"
                | "type_switch_statement"
                | "expression_switch_statement"
        )
    }

    /// 函数节点：函数声明 / 方法声明 / 函数字面量（闭包，tree-sitter-go 中
    /// 节点名为 `func_literal`）。
    ///
    /// 闭包（`func() { ... }`）是独立作用域，必须识别为函数节点，
    /// 否则其内部 if 会被 `walk_if_stats` 的嵌套函数边界守卫当成外层
    /// 函数的兄弟语句，虚增外层函数的嵌套深度（SPEC §2 嵌套深度语义）。
    fn is_function_node(&self, kind: &str) -> bool {
        matches!(
            kind,
            "function_declaration" | "method_declaration" | "func_literal"
        )
    }

    fn is_if_node(&self, kind: &str) -> bool {
        kind == "if_statement"
    }

    /// switch 的每个 case 各计一个决策点（与 rust match / js switch 对齐）。
    fn count_branches<'tree>(&self, node: &Node<'tree>) -> Option<u32> {
        match node.kind() {
            "expression_switch_statement" | "type_switch_statement" => {
                let mut count = 0;
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "expression_case" | "type_case" | "default_case" => count += 1,
                        _ => {}
                    }
                }
                Some(count)
            }
            _ => None,
        }
    }

    fn is_chained_else_if<'tree>(&self, node: &Node<'tree>) -> bool {
        // Go 的 else if 在 AST 里同样是 "else 分支只包一个 if" 的模式，
        // 复用公共实现。带初始化语句的 `if x := f(); x != nil`：init 是
        // if 节点自己的 initializer 字段，属于 if 内部而不是 else 分支的
        // 兄弟语句，不会影响"是否为纯链式"的判定。
        chained_else_if("if_statement", "block", node)
    }
}

/// 在给定文本中统计 Go 断言调用（已去除字符串/注释，按调用表达式识别）。
///
/// 覆盖两套惯用法：
/// - testify：`require.Xxx` / `assert.Xxx`（任意方法都算断言）；
/// - testing 包：`t.Error*` / `t.Fatal*` / `t.Fail*`（排除 `fmt.Errorf`
///   这类"构造错误"而不是"断言失败"的调用）。
fn is_go_assertion_call(callee: &str) -> bool {
    let callee = callee.trim();
    let Some((receiver, method)) = callee.rsplit_once('.') else {
        return false;
    };
    if matches!(receiver, "require" | "assert") {
        return true;
    }
    matches!(
        method,
        "Error" | "Errorf" | "Fatal" | "Fatalf" | "Fail" | "FailNow"
    ) && !matches!(receiver, "fmt" | "errors" | "xerrors" | "wrapping")
}

/// 统计 Go 测试函数（`func TestXxx`，含 `t.Run` 子测试闭包）体内的断言数。
pub fn go_assertion_count(source: &str) -> u32 {
    let Ok(tree) = parse_source(Language::Go, source) else {
        return 0;
    };
    let mut count = 0;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_declaration" {
            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or("");
            // 只统计以 Test 开头的测试函数；其子树内的 t.Run 子测试闭包
            // 会被一并覆盖（闭包是测试函数体的后代节点）
            if name.starts_with("Test") {
                if let Some(body) = node.child_by_field_name("body") {
                    count += go_assertions_in_subtree(&body, source);
                }
            }
            // 不深入命名函数内部，避免嵌套函数重复计数
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    count
}

/// 在子树内统计断言调用表达式数。
fn go_assertions_in_subtree(root: &Node<'_>, source: &str) -> u32 {
    let mut count = 0;
    let mut stack = vec![*root];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            if let Some(func) = node.child_by_field_name("function") {
                if let Ok(text) = func.utf8_text(source.as_bytes()) {
                    if is_go_assertion_call(text) {
                        count += 1;
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    count
}

/// 把 `go tool cover` 输出转换为 Cobertura XML。
///
/// `go tool cover` 格式：首行 `mode: set|count|atomic`，随后每条语句一行：
/// `file.go:startLine.startCol,endLine.endCol numStmt count`。
/// 转成 Cobertura 后复用 rustburn 已有的 cobertura 解析器（语句级覆盖率，
/// 与 `go tool cover -func` 报告的语义一致）。
///
/// `module`：go.mod 里的 module 路径（如 `example.com/gotest`）。`go tool
/// cover` 输出的文件路径带模块前缀（`example.com/gotest/main.go`），而
/// 扫描出的文件路径是仓库相对路径（`main.go`），必须剥离前缀才能对上。
pub fn go_cover_to_cobertura(content: &str, module: Option<&str>) -> String {
    // file -> (语句序号, 是否命中)
    let mut files: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    let mut seq = 0u32;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("mode:") {
            continue;
        }
        // `file:start.end,end.end numStmt count`
        let Some((loc, rest)) = line.split_once(' ') else {
            continue;
        };
        let Some((file, _range)) = loc.rsplit_once(':') else {
            continue;
        };
        let Some((_num_stmt, count_str)) = rest.trim().split_once(' ') else {
            continue;
        };
        let count: u32 = count_str.trim().parse().unwrap_or(0);
        let mut file = file.to_string();
        // 剥离模块前缀：`{module}/main.go` → `main.go`
        if let Some(module) = module {
            if let Some(stripped) = file.strip_prefix(module).and_then(|s| s.strip_prefix('/')) {
                file = stripped.to_string();
            }
        }
        seq += 1;
        files
            .entry(file)
            .or_default()
            .push((seq, u32::from(count > 0)));
    }

    let mut xml = String::from("<coverage>\n<packages><package><classes>\n");
    let mut sorted: Vec<_> = files.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (file, stmts) in sorted {
        xml.push_str(&format!(
            "<class filename=\"{}\">\n<lines>\n",
            xml_escape(&file)
        ));
        for (num, hits) in stmts {
            xml.push_str(&format!("<line number=\"{}\" hits=\"{}\"/>\n", num, hits));
        }
        xml.push_str("</lines>\n</class>\n");
    }
    xml.push_str("</classes></package></packages>\n</coverage>\n");
    xml
}

/// 从 go.mod 读取 module 路径（如 `module example.com/gotest`）。
pub fn go_module_from_gomod(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .find_map(|l| {
            l.strip_prefix("module ")
                .map(|m| m.split_whitespace().next().unwrap_or("").to_string())
        })
        .filter(|m| !m.is_empty())
}

/// 极简 XML 转义（覆盖率文件路径几乎不含特殊字符，防御性处理引号）。
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzers::complexity::compute_metrics;
    use crate::analyzers::test::parse_cobertura;

    fn depth_of(src: &str) -> u32 {
        let adapter = GoAdapter;
        let tree = adapter.parse(src).expect("parse ok");
        compute_metrics(&tree, src, &adapter).max_if_nesting_depth
    }

    fn metrics_of(src: &str) -> crate::analyzers::complexity::FileComplexity {
        let adapter = GoAdapter;
        let tree = adapter.parse(src).expect("parse ok");
        compute_metrics(&tree, src, &adapter)
    }

    /// 纯链式 else-if：`else if` 不增加嵌套深度（深度应为 1）。
    #[test]
    fn test_pure_chain_else_if_depth_is_one() {
        let src = r#"
package main
func check(x int) {
    if x > 0 {
        println("a")
    } else if x < 0 {
        println("b")
    } else if x == 0 {
        println("c")
    } else {
        println("d")
    }
}
"#;
        assert_eq!(depth_of(src), 1, "纯链式 else-if 嵌套深度应为 1");
    }

    /// 带初始化语句的 if：`if x := f(); x != nil` 的 init 是 if 节点自己的
    /// 字段，不是 else 分支的兄弟语句，不得影响链式判定（深度仍为 1）。
    #[test]
    fn test_init_statement_chain_not_inflated() {
        let src = r#"
package main
func handle(v *V) {
    if x := v.field(); x != nil {
        println("has")
    } else if y := v.other(); y != nil {
        println("other")
    } else {
        println("none")
    }
}
"#;
        let m = metrics_of(src);
        assert_eq!(
            m.max_if_nesting_depth, 1,
            "带 init 的链式不应把初始化语句误判为兄弟语句而增加深度"
        );
        // 注：nested_if_ratio 对链式 else-if 的统计与 rust/js 一致（链式
        // 子 if 计入 nested），属既有跨语言行为，不在本次 Go 支持范围内。
    }

    /// 真实嵌套按层数递增。
    #[test]
    fn test_true_nesting_depth() {
        let src = r#"
package main
func check(x int) {
    if x > 0 {
        if x > 10 {
            if x > 100 {
                println("deep")
            }
        }
    }
}
"#;
        assert_eq!(depth_of(src), 3, "真实三层嵌套深度应为 3");
    }

    /// 混合：外层链式 + 内层真实嵌套。
    #[test]
    fn test_mixed_chain_and_nesting() {
        let src = r#"
package main
func check(x, y int) {
    if x > 0 {
        if y > 0 {
            println("a")
        }
    } else if x < 0 {
        println("b")
    }
}
"#;
        assert_eq!(depth_of(src), 2);
    }

    /// Go switch：每个 case 各计一个决策点。
    #[test]
    fn test_switch_cases_count_as_branches() {
        let src = r#"
package main
func pick(x int) string {
    switch x {
    case 1:
        return "one"
    case 2:
        return "two"
    default:
        return "other"
    }
}
"#;
        let m = metrics_of(src);
        // 3 个 case → 圈复杂度 = 1 + 3 = 4
        assert_eq!(m.cyclomatic_complexity, 4, "3 个 case 应为 4");
    }

    /// 接口方法声明没有函数体，不得产生虚假复杂度。
    #[test]
    fn test_interface_methods_do_not_add_complexity() {
        let src = r#"
package main
type Reader interface {
    Read(p []byte) (int, error)
    Close() error
}
func f() int {
    return 1
}
"#;
        let m = metrics_of(src);
        assert_eq!(m.cyclomatic_complexity, 1, "接口方法不应计为函数体");
        assert_eq!(m.max_if_nesting_depth, 0);
    }

    /// goroutine 闭包是独立作用域：闭包内的 if 不得算进外层函数的嵌套深度。
    ///
    /// 若无嵌套函数边界守卫，闭包内 `if b { if c }` 会被吸收进外层
    /// `if a` 的子树，虚增最大深度到 3；守卫生效时闭包按自身作用域
    /// 统计，文件级最大深度为 2。
    #[test]
    fn test_goroutine_closure_if_not_in_outer_depth() {
        let src = r#"
package main
func outer() {
    if a {
        go func() {
            if b {
                if c {
                    println("deep")
                }
            }
        }()
    }
}
"#;
        assert_eq!(depth_of(src), 2, "闭包内 if 不应被算进外层函数的深度");
    }

    /// Go 断言计数：testify require/assert、t.Errorf 计入；t.Log 不计入；
    /// fmt.Errorf 是错误构造不是断言。
    #[test]
    fn test_go_assertion_count() {
        let src = r#"
package main
import (
    "testing"
    "github.com/stretchr/testify/require"
)
func TestAdd(t *testing.T) {
    got := add(1, 2)
    require.Equal(t, 3, got)
    if got != 3 {
        t.Errorf("want 3, got %d", got)
    }
    t.Log("finished")
    err := fmt.Errorf("boom")
    _ = err
}
func TestSub(t *testing.T) {
    assert.NoError(t, nil)
    t.Run("nested", func(t *testing.T) {
        t.Fatalf("boom")
    })
}
"#;
        // require.Equal(1) + t.Errorf(1) + assert.NoError(1) + t.Fatalf(1) = 4
        assert_eq!(go_assertion_count(src), 4, "Go 断言数应为 4");
    }

    /// go tool cover 输出 → Cobertura → 复用现有解析器得到语句覆盖率。
    #[test]
    fn test_go_cover_to_cobertura() {
        let cover = "mode: set\nfoo.go:1.1,3.2 3 1\nfoo.go:5.1,6.2 2 0\nbar.go:1.1,2.2 2 1\n";
        let xml = go_cover_to_cobertura(cover, None);
        let cov = parse_cobertura(&xml);
        assert!((cov["foo.go"] - 50.0).abs() < 0.01, "foo.go 3/2 语句命中");
        assert!((cov["bar.go"] - 100.0).abs() < 0.01, "bar.go 全部命中");
    }

    /// go tool cover 的路径带模块前缀，转换时需剥离才能对上仓库相对路径。
    #[test]
    fn test_go_cover_strips_module_prefix() {
        let cover = "mode: set\nexample.com/gotest/main.go:1.1,2.2 2 1\n";
        let xml = go_cover_to_cobertura(cover, Some("example.com/gotest"));
        let cov = parse_cobertura(&xml);
        assert!(cov.contains_key("main.go"), "应剥离模块前缀");
        assert!((cov["main.go"] - 100.0).abs() < 0.01);
        assert!(!cov.contains_key("example.com/gotest/main.go"));
    }

    #[test]
    fn test_go_module_from_gomod() {
        let gomod = "module example.com/gotest\n\ngo 1.21\n";
        assert_eq!(
            go_module_from_gomod(gomod).as_deref(),
            Some("example.com/gotest")
        );
        assert_eq!(go_module_from_gomod("// no module"), None);
    }
}
