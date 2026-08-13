//! build.rs：注入 git commit 短哈希与构建日期，供 `rb --version` 展示。
//!
//! 两者均为尽力而为：git 不可用 / 无提交历史时回退为 "unknown"，
//! 保证任何环境下都能编译通过。

use std::process::Command;

fn cmd_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // 从 rustburn-cli 目录向上定位 git 仓库根并取当前 HEAD 短哈希
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let commit = cmd_output("git", &["-C", manifest_dir, "rev-parse", "--short", "HEAD"]);
    // 最近一次提交的日期（ISO-8601，仅日期部分）作为构建日期
    let build_date = cmd_output("git", &["-C", manifest_dir, "log", "-1", "--format=%cs"]);

    println!("cargo:rustc-env=RUSTBURN_GIT_COMMIT={}", commit);
    println!("cargo:rustc-env=RUSTBURN_BUILD_DATE={}", build_date);
}
