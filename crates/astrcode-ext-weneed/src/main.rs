//! 扩展入口。stdout 只允许 S5R 协议帧，错误写 stderr。

#[tokio::main]
async fn main() {
    if let Err(error) = astrcode_ext_weneed::run().await {
        eprintln!("astrcode-ext-weneed 退出：{error}");
        std::process::exit(1);
    }
}
