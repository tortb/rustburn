pub mod model;

/// rustburn-core 占位模块，将在后续 Phase 中实现。
pub mod complexity;
pub mod dependency;
pub mod git_history;
pub mod scoring;
pub mod update;

/// 调试日志是否启用（环境变量 `RB_DEBUG` 非空）。
///
/// 调用方应先判断此函数再构造日志参数，避免默认路径上无谓的格式化开销。
pub fn debug_enabled() -> bool {
    std::env::var_os("RB_DEBUG").is_some()
}

/// 调试日志：仅当环境变量 `RB_DEBUG` 非空时输出到 stderr。
///
/// 用于排查分数/数据异常（如 `RB_DEBUG=1 rb scan .`），
/// 不引入任何第三方日志依赖，默认零开销。
pub fn debug_log(args: std::fmt::Arguments<'_>) {
    if debug_enabled() {
        eprintln!("[rb-debug] {}", args);
    }
}
