//! 生成一个样例 .apkg 到 `/tmp/astrcode-anki-deck-sample/sample.apkg`，供人工导入
//! Anki 实测或用外部工具（如 Python sqlite3）交叉验证。
//!
//! 运行：`cargo run -p astrcode-ext-anki-deck --example gen_sample`

use std::path::Path;

use astrcode_ext_anki_deck::apkg::{self, DeckPlan, PlanCard};
use astrcode_ext_anki_deck::guid::guid_for;

fn main() {
    let dir = Path::new("/tmp/astrcode-anki-deck-sample");
    std::fs::create_dir_all(dir).expect("建输出目录");

    // 一张样例媒体文件，验证媒体打包路径。
    let media = dir.join("sample.png");
    std::fs::write(&media, b"fake-png-bytes").expect("写样例媒体");

    let guid = |id: &str| guid_for(&["Rust::所有权", "id", id]);
    let plan = DeckPlan {
        name: String::from("Rust::所有权"),
        css: None,
        cards: vec![
            PlanCard {
                front: String::from("所有权规则中，同一时刻值可以有几个人所有？"),
                back: String::from("恰好<b>一个</b>（moving 会转移所有权）。"),
                tags: vec![String::from("rust"), String::from("ownership")],
                cloze: false,
                guid: guid("ownership-rule"),
            },
            PlanCard {
                front: String::from("Drop 在 {{c1::作用域结束}} 时被调用"),
                back: String::from("RAII 的关键。"),
                tags: vec![String::from("rust"), String::from("drop")],
                cloze: true,
                guid: guid("drop-timing"),
            },
        ],
        media: vec![apkg::MediaEntry {
            source: media,
            basename: String::from("sample.png"),
        }],
    };

    let output = dir.join("sample.apkg");
    let stats = apkg::write_apkg(&plan, &output).expect("写 apkg");
    println!(
        "已生成 {}（notes={}, cloze={}, cards={}, media={}）",
        output.display(),
        stats.notes,
        stats.cloze_notes,
        stats.cards,
        stats.media,
    );
}
