//! 扩展入口。stdout 只允许 S5R 协议帧，错误写 stderr。

#[tokio::main]
async fn main() {
    if let Err(error) = astrcode_ext_cache_doctor::run().await {
        eprintln!("astrcode-ext-cache-doctor 退出：{error}");
        std::process::exit(1);
    }
}
