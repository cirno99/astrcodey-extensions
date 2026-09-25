//! `astrcode-ext-cache-usage` 的可执行入口。
//!
//! 宿主按 `extension.json` 的 `command` 启动本进程，经 stdio 用 S5R 3.0 帧通信。

#[tokio::main]
async fn main() {
    if let Err(error) = astrcode_ext_cache_usage::run().await {
        // stdout 专用于 S5R 帧；诊断信息只能写 stderr。
        eprintln!(
            "{}: {} ({})",
            astrcode_ext_cache_usage::command::EXTENSION_ID,
            error.message,
            error.code
        );
        std::process::exit(1);
    }
}
