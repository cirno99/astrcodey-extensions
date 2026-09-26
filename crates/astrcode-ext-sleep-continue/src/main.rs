//! 扩展入口。stdout 只允许 S5R 协议帧，错误写 stderr。

#[tokio::main]
async fn main() {
    if let Err(error) = astrcode_ext_sleep_continue::run().await {
        eprintln!("astrcode-ext-sleep-continue 退出：{error}");
        std::process::exit(1);
    }
}
