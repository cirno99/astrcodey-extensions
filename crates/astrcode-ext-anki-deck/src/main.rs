//! `astrcode-ext-anki-deck` 的可执行入口。
//!
//! stdout 专用于 S5R 帧，日志一律走 stderr（宿主会持续 drain 但不转发）。

#[tokio::main]
async fn main() {
    if let Err(error) = astrcode_ext_anki_deck::run().await {
        eprintln!(
            "astrcode-anki-deck failed: {} ({})",
            error.message, error.code
        );
        std::process::exit(1);
    }
}
