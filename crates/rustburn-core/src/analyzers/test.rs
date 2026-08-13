//! TestAnalyzer：测试覆盖与质量维度。
//!
//! 不依赖 [crate::lang::LanguageAdapter]，走文件命名约定 + 覆盖率报告。
//!
//! 公式（SPEC v2 §4）：
//! ```text
//! test_risk_value = 覆盖率缺口 × 0.5 + 测试密度缺口 × 0.3 + 断言密度缺口 × 0.2
//! 覆盖率缺口 = 100 - 覆盖率%（仅当能解析到 lcov/cobertura 报告时使用）
//! 测试密度缺口 = 100 - min(100, 对应测试文件总行数 / 该文件行数 × 100)
//! 断言密度缺口 = 100 - min(100, 测试文件里 assert/expect 类调用数 / 测试文件行数 × 20)
//! ```

use std::collections::HashMap;

use regex::Regex;
use serde_json::json;

use crate::analyzer::DimensionAnalyzer;
use crate::context::{FileContext, TestFileStats, TestRepoContext};
use crate::model::{Confidence, DimensionResult, Language};

/// 覆盖缺失时无任何仓库数据的兜底值（中性，不代表 0 或 100 风险）。
const NEUTRAL_MISSING: f64 = 50.0;

/// 测试文件路径映射规则（SPEC v2 §4.2 规则 3：可配置正则，不硬编码死路径）。
#[derive(Debug, Clone)]
pub struct TestPathRules {
    /// 匹配 tests/ 目录下测试文件的正则，捕获组 1 = 主干名（含 _test 后缀）
    pub tests_dir_pattern: Regex,
    /// 候选源根目录（按顺序尝试）
    pub source_roots: Vec<String>,
}

impl Default for TestPathRules {
    fn default() -> Self {
        Self {
            tests_dir_pattern: Regex::new(r"^tests/(.+)\.(rs|js|jsx)$")
                .expect("tests dir pattern valid"),
            source_roots: vec!["src".to_string(), "lib".to_string(), "app".to_string()],
        }
    }
}

/// 输入文件的最小视图（供注册表构建使用）。
#[derive(Debug, Clone)]
pub struct TestFileInput {
    pub path: String,
    pub source: String,
    pub language: Language,
    pub loc: u32,
}

/// 解析 lcov 报告，返回 文件相对路径 → 覆盖率（0-100）。
pub fn parse_lcov(content: &str) -> HashMap<String, f64> {
    let mut coverage = HashMap::new();
    let mut current_sf: Option<String> = None;
    let mut lf: f64 = 0.0;
    let mut lh: f64 = 0.0;

    for line in content.lines() {
        let line = line.trim();
        if let Some(sf) = line.strip_prefix("SF:") {
            current_sf = Some(normalize_lcov_path(sf));
        } else if let Some(v) = line.strip_prefix("LF:") {
            lf = v.parse().unwrap_or(0.0);
        } else if let Some(v) = line.strip_prefix("LH:") {
            lh = v.parse().unwrap_or(0.0);
        } else if line == "end_of_record" {
            if let Some(sf) = current_sf.take() {
                if lf > 0.0 {
                    coverage.insert(sf, (lh / lf * 100.0).clamp(0.0, 100.0));
                }
            }
        }
    }
    coverage
}

/// 归一化 lcov 中的文件路径为仓库相对路径。
fn normalize_lcov_path(sf: &str) -> String {
    let sf = sf.trim().trim_start_matches("./");
    sf.trim_start_matches('/').to_string()
}

/// 解析 cobertura XML 报告，返回 文件相对路径 → 覆盖率（0-100）。
pub fn parse_cobertura(content: &str) -> HashMap<String, f64> {
    let mut coverage = HashMap::new();
    let mut current_class: Option<String> = None;
    let mut total_lines: f64 = 0.0;
    let mut hit_lines: f64 = 0.0;

    for line in content.lines() {
        let line = line.trim();
        if let Some(start) = line.find("class filename=\"") {
            let rest = &line[start + "class filename=\"".len()..];
            if let Some(end) = rest.find('"') {
                current_class = Some(normalize_lcov_path(&rest[..end]));
            }
        } else if line.starts_with("<line ") && line.contains("hits=") {
            total_lines += 1.0;
            // 提取 hits="N"
            if let Some(hs) = line.find("hits=\"") {
                let rest = &line[hs + "hits=\"".len()..];
                if let Some(he) = rest.find('"') {
                    let hits: f64 = rest[..he].parse().unwrap_or(0.0);
                    if hits > 0.0 {
                        hit_lines += 1.0;
                    }
                }
            }
        } else if line.contains("</class>") {
            if let Some(class) = current_class.take() {
                if total_lines > 0.0 {
                    coverage.insert(class, (hit_lines / total_lines * 100.0).clamp(0.0, 100.0));
                }
                total_lines = 0.0;
                hit_lines = 0.0;
            }
        }
    }
    coverage
}

/// 常见覆盖率报告路径（仓库根相对）。
const COVERAGE_FILE_CANDIDATES: &[&str] = &[
    "lcov.info",
    "coverage/lcov.info",
    "coverage/lcov-report/lcov.info",
    "coverage.xml",
    "coverage/coverage.xml",
    "cobertura.xml",
    "coverage/cobertura.xml",
    "target/coverage/lcov.info",
];

/// 从仓库读取覆盖率报告。
///
/// 返回值：覆盖率报告原文（按优先级取第一个找到的）。
pub fn read_coverage_report(repo_path: &std::path::Path) -> Option<String> {
    for candidate in COVERAGE_FILE_CANDIDATES {
        let path = repo_path.join(candidate);
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                return Some(content);
            }
        }
    }
    None
}

/// 构建测试上下文（测试文件注册表 + 覆盖率）。
///
/// `coverage_content` 为可选覆盖率报告原文（lcov 或 cobertura，自动识别）。
pub fn build_test_context(
    files: &[TestFileInput],
    coverage_content: Option<&str>,
    rules: &TestPathRules,
) -> TestRepoContext {
    let mut ctx = TestRepoContext::default();

    // 覆盖率
    let coverage = coverage_content.map_or_else(HashMap::new, |c| {
        if c.trim_start().starts_with('<') {
            parse_cobertura(c)
        } else {
            parse_lcov(c)
        }
    });
    ctx.coverage = coverage;

    // 有覆盖率数据的文件其覆盖率缺口均值
    let gaps: Vec<f64> = ctx.coverage.values().map(|c| 100.0 - c).collect();
    if !gaps.is_empty() {
        ctx.mean_coverage_gap = Some(gaps.iter().sum::<f64>() / gaps.len() as f64);
    }

    // 收集测试文件（按命名约定识别）
    let by_path: HashMap<&str, &TestFileInput> =
        files.iter().map(|f| (f.path.as_str(), f)).collect();

    // Rule 1 & 2 & 3 依次为每个文件找对应测试
    for file in files {
        // Rule 1：同目录命名约定
        let candidates = same_dir_test_candidates(&file.path);
        let mut matched: Vec<&TestFileInput> = Vec::new();
        for cand in &candidates {
            if let Some(t) = by_path.get(cand.as_str()) {
                matched.push(*t);
            }
        }
        if !matched.is_empty() {
            let stats = matched
                .into_iter()
                .map(|t| TestFileStats {
                    path: t.path.clone(),
                    test_loc: t.source.lines().count().max(1) as u32,
                    impl_loc: file.loc.max(1),
                    assertion_count: count_assertions(&t.source, t.language),
                })
                .collect();
            ctx.test_files.insert(file.path.clone(), stats);
            continue;
        }

        // Rule 2：Rust 内部 #[cfg(test)] mod tests
        if file.language == Language::Rust {
            if let Some((mod_loc, assertions)) = rust_test_mod_stats(&file.source) {
                let stats = vec![TestFileStats {
                    path: file.path.clone(),
                    test_loc: mod_loc,
                    impl_loc: file.loc.saturating_sub(mod_loc).max(1),
                    assertion_count: assertions,
                }];
                ctx.test_files.insert(file.path.clone(), stats);
                continue;
            }
        }

        // Rule 3：tests/ 目录正则映射
        if let Some(stats) = match_tests_dir(file, files, rules) {
            ctx.test_files.insert(file.path.clone(), stats);
        }
    }

    ctx
}

/// 同目录测试文件命名候选：`<stem>_test.<ext>` / `<stem>.test.<ext>` / `test_<stem>.<ext>`。
fn same_dir_test_candidates(path: &str) -> Vec<String> {
    let (dir, file_name) = match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    };
    let Some((stem, ext)) = file_name.rsplit_once('.') else {
        return Vec::new();
    };
    let dir_prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{}/", dir)
    };
    vec![
        format!("{}{}_test.{}", dir_prefix, stem, ext),
        format!("{}{}.test.{}", dir_prefix, stem, ext),
        format!("{}test_{}.{}", dir_prefix, stem, ext),
    ]
}

/// 提取 Rust 文件中 `#[cfg(test)] mod tests` 块：返回 (mod 行数, 断言数)。
fn rust_test_mod_stats(source: &str) -> Option<(u32, u32)> {
    let stripped = strip_comments(source);
    let attr_pos = stripped.find("#[cfg(test)]")?;
    let after = &stripped[attr_pos..];
    let brace_pos = after.find('{')? + attr_pos;
    let close = match_brace(stripped.as_bytes(), brace_pos)?;
    let line_count =
        |up_to: usize| stripped[..up_to].bytes().filter(|&b| b == b'\n').count() as u32;
    let start_row = line_count(brace_pos);
    let end_row = line_count(close);
    let mod_source = mod_source_slice(source, brace_pos, close);
    let assertions = count_assert_patterns(mod_source);
    Some((end_row - start_row + 1, assertions))
}

fn mod_source_slice(source: &str, brace_pos: usize, close: usize) -> &str {
    if brace_pos < close && close <= source.len() {
        &source[brace_pos..close]
    } else {
        ""
    }
}

/// tests/ 目录映射：用可配置正则推导候选实现路径。
fn match_tests_dir(
    file: &TestFileInput,
    files: &[TestFileInput],
    rules: &TestPathRules,
) -> Option<Vec<TestFileStats>> {
    let mut matched = Vec::new();
    for t in files {
        if !t.path.starts_with("tests/") {
            continue;
        }
        let caps = rules.tests_dir_pattern.captures(&t.path)?;
        let mut stem = caps.get(1)?.as_str().to_string();
        let ext = caps.get(2)?.as_str().to_string();
        // 去掉测试命名标记
        for marker in ["_test", ".test"] {
            if let Some(stripped) = stem.strip_suffix(marker) {
                stem = stripped.to_string();
                break;
            }
        }
        for root in &rules.source_roots {
            let candidate = if root == "." {
                format!("{}.{}", stem, ext)
            } else {
                format!("{}/{}.{}", root, stem, ext)
            };
            if candidate == file.path {
                matched.push(TestFileStats {
                    path: t.path.clone(),
                    test_loc: t.source.lines().count().max(1) as u32,
                    impl_loc: file.loc.max(1),
                    assertion_count: count_assertions(&t.source, t.language),
                });
                break;
            }
        }
    }
    if matched.is_empty() {
        None
    } else {
        Some(matched)
    }
}

/// 统计测试文件中位于测试函数体内的断言调用数（上下文感知，SPEC v2 §4 禁止事项 4-B）。
pub fn count_assertions(source: &str, lang: Language) -> u32 {
    let bodies = match lang {
        Language::Rust => rust_test_bodies(source),
        Language::JavaScript | Language::Mock => js_test_bodies(source),
        Language::Unknown => Vec::new(),
    };
    bodies
        .iter()
        .map(|(start, end)| count_assert_patterns(&source[*start..*end]))
        .sum()
}

/// 在给定文本中统计 assert/expect 类调用出现次数（已去除注释）。
fn count_assert_patterns(text: &str) -> u32 {
    let stripped = strip_comments(text);
    let lower = stripped.to_lowercase();
    let mut count = 0;
    for pattern in ["assert", "expect"] {
        let mut pos = 0;
        while let Some(idx) = lower[pos..].find(pattern) {
            let global = pos + idx;
            // 只统计单词开头（避免函数名中的 assert 拼写误计）
            let before_ok = global == 0
                || !lower.as_bytes()[global - 1].is_ascii_alphanumeric()
                    && lower.as_bytes()[global - 1] != b'_';
            if before_ok {
                count += 1;
            }
            pos = global + pattern.len();
        }
    }
    count
}

/// 找出 Rust 测试函数体：`#[test]` 标注或 `test_` 前缀函数。
fn rust_test_bodies(source: &str) -> Vec<(usize, usize)> {
    let stripped = strip_comments(source);
    let bytes = stripped.as_bytes();
    let mut bodies = Vec::new();
    let mut i = 0;
    let mut pending_attr = false;
    while i < bytes.len() {
        if stripped[i..].starts_with("#[test") {
            pending_attr = true;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if is_keyword_at(bytes, i, b"fn") {
            let mut name_start = i + 2;
            while name_start < bytes.len() && bytes[name_start] == b' ' {
                name_start += 1;
            }
            let mut name_end = name_start;
            while name_end < bytes.len()
                && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_')
            {
                name_end += 1;
            }
            let name = &stripped[name_start..name_end];
            if pending_attr || name.starts_with("test_") {
                if let Some(body_start) = find_byte(bytes, i, b'{') {
                    if let Some(body_end) = match_brace(bytes, body_start) {
                        bodies.push((body_start, body_end));
                    }
                }
            }
            pending_attr = false;
            i = name_end;
            continue;
        }
        i += 1;
    }
    bodies
}

/// 找出 JS 测试体：`test(` / `it(` / `describe(` 调用的括号区间。
fn js_test_bodies(source: &str) -> Vec<(usize, usize)> {
    let stripped = strip_comments(source);
    let bytes = stripped.as_bytes();
    let mut bodies = Vec::new();
    for marker in ["test(", "it(", "describe("] {
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(marker.as_bytes()) {
                // 单词边界：前面不能是字母/下划线（避免 latest(、attribute(）
                let prev_ok =
                    i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
                if prev_ok {
                    let open = i + marker.len() - 1; // '('
                    if let Some(close) = match_paren(bytes, open) {
                        bodies.push((open, close));
                        i = close + 1;
                        continue;
                    }
                }
            }
            i += 1;
        }
    }
    bodies
}

/// 是否在位置 `i` 处出现关键字（前后为词边界）。
fn is_keyword_at(bytes: &[u8], i: usize, kw: &[u8]) -> bool {
    if !bytes[i..].starts_with(kw) {
        return false;
    }
    let before_ok = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
    let after = bytes.get(i + kw.len()).copied().unwrap_or(b' ');
    let after_ok = !after.is_ascii_alphanumeric() && after != b'_';
    before_ok && after_ok
}

/// 从 `from` 起查找第一个字节 `b`。
fn find_byte(bytes: &[u8], from: usize, b: u8) -> Option<usize> {
    bytes[from..].iter().position(|&x| x == b).map(|p| from + p)
}

/// 去除字符串与注释内容（替换为等长空格，保留行结构）。
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = source.as_bytes().to_vec();
    let mut i = 0;
    let mut in_line = false;
    let mut in_block = false;
    let mut in_str = false;
    let mut sc = 0u8;
    while i < bytes.len() {
        let b = bytes[i];
        if in_line {
            out[i] = b' ';
            if b == b'\n' {
                in_line = false;
            }
            i += 1;
            continue;
        }
        if in_block {
            out[i] = b' ';
            if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                out[i + 1] = b' ';
                in_block = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_str {
            if b == sc {
                in_str = false;
            } else if b == b'\\' {
                i += 1;
            }
            i += 1;
            continue;
        }
        match b {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                in_line = true;
                out[i] = b' ';
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                in_block = true;
                out[i] = b' ';
                i += 1;
            }
            b'"' | b'\'' | b'`' => {
                in_str = true;
                sc = b;
            }
            _ => {}
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// 匹配 `{`（位置 open）对应的 `}`。
fn match_brace(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 匹配 `(`（位置 open）对应的 `)`。
fn match_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// TestAnalyzer：不依赖 LanguageAdapter，走文件命名约定 + 覆盖率报告。
pub struct TestAnalyzer;

/// 计算单个文件在有测试文件时的风险分；无对应测试文件返回 None。
pub fn compute_risk(ctx: &FileContext<'_>) -> Option<f64> {
    let stats = ctx.repo.test.test_files.get(ctx.path)?;
    if stats.is_empty() {
        return None;
    }
    let test_loc: u32 = stats.iter().map(|s| s.test_loc).sum();
    let impl_loc: u32 = stats[0].impl_loc.max(1);
    let assertions: u32 = stats.iter().map(|s| s.assertion_count).sum();

    let coverage = ctx.repo.test.coverage.get(ctx.path).copied();
    let coverage_gap = match coverage {
        Some(c) => 100.0 - c,
        None => ctx.repo.test.mean_coverage_gap.unwrap_or(NEUTRAL_MISSING),
    };
    let density_gap = 100.0 - (test_loc as f64 / impl_loc as f64 * 100.0).min(100.0);
    let assertion_gap = 100.0 - (assertions as f64 / test_loc.max(1) as f64 * 20.0).min(100.0);
    Some((coverage_gap * 0.5 + density_gap * 0.3 + assertion_gap * 0.2).clamp(0.0, 100.0))
}

impl DimensionAnalyzer for TestAnalyzer {
    fn name(&self) -> &'static str {
        "test"
    }

    fn analyze(&self, ctx: &FileContext<'_>) -> DimensionResult {
        // 找不到对应测试文件（SPEC v2 §4.2 规则 4）：
        // 覆盖率缺口按仓库均值填充（禁止硬编码 0/100，禁止事项 4-A），
        // 测试密度缺口与断言密度缺口天然为 100（没有任何测试代码）。
        let Some(risk) = compute_risk(ctx) else {
            let coverage_gap = ctx.repo.test.mean_coverage_gap.unwrap_or(NEUTRAL_MISSING);
            let risk = (coverage_gap * 0.5 + 30.0 + 20.0).clamp(0.0, 100.0);
            return DimensionResult {
                raw_value: risk,
                risk_score: risk,
                confidence: Confidence::DataMissing("未找到对应测试文件".to_string()),
                detail: json!({
                    "reason": "未找到对应测试文件",
                    "coverage_gap_filled_with": "repo_mean_or_neutral",
                }),
            };
        };

        // 有测试文件但缺少覆盖率报告 → 覆盖率缺口按仓库均值填充，标记数据缺失
        let coverage = ctx.repo.test.coverage.get(ctx.path).copied();
        let confidence = if coverage.is_some() {
            Confidence::Full
        } else {
            Confidence::DataMissing("缺少覆盖率报告".to_string())
        };

        let stats = &ctx.repo.test.test_files[ctx.path];
        let test_loc: u32 = stats.iter().map(|s| s.test_loc).sum();
        let assertions: u32 = stats.iter().map(|s| s.assertion_count).sum();

        DimensionResult {
            raw_value: risk,
            risk_score: risk,
            confidence,
            detail: json!({
                "test_files": stats.iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
                "test_loc": test_loc,
                "assertion_count": assertions,
                "coverage": coverage,
            }),
        }
    }
}
