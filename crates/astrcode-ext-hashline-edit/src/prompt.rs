//! 注入 system prompt 的引导语。
//!
//! 原版通过 DSH 的 `systemPrompt.section()` 常驻一段说明，让模型知道有 hashline 工具、
//! 以及「优先用 replace 而不是 edit」。AstrCode 走 `prompt_build` 钩子，宿主把贡献映射成
//! `ExtensionSection::PlatformInstructions`，落在 system prompt 的**静态前缀区**，因此只在
//! 贡献变化时让 provider 前缀缓存失效。
//!
//! 正文与原版逐字一致（英文）。它是被调优过的提示词工件，翻译会改变语义。

/// 注入到 system prompt 的引导语。
pub const GUIDANCE: &str = "Hash-anchored editing is available: use hashline_read to read a file as HASH│content rows (unique 3-char anchors per line, no line numbers), then replace a line range with the replace tool using bare hashes in remove_from/remove_to. Anchors of untouched lines stay valid across replaces, so edits chain without re-reading. undo_last_replace reverts the last replace on a file. Pass raw:true to hashline_read for plain numbered output (inspection only). The built-in read/edit/write tools remain available for other purposes; prefer replace for targeted edits so stale-anchor corruption is impossible.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_names_all_three_tools() {
        assert!(GUIDANCE.contains("hashline_read"));
        assert!(GUIDANCE.contains("replace"));
        assert!(GUIDANCE.contains("undo_last_replace"));
    }

    #[test]
    fn guidance_explains_the_bare_hash_contract() {
        assert!(GUIDANCE.contains("bare hashes in remove_from/remove_to"));
    }

    #[test]
    fn guidance_keeps_the_anchor_row_separator() {
        // 引导语里的 `HASH│content` 用的是同一个分隔符，模型据此理解 read 的输出格式
        assert!(GUIDANCE.contains("HASH\u{2502}content"));
    }

    #[test]
    fn guidance_steers_away_from_the_builtin_edit_tool() {
        assert!(GUIDANCE.contains("prefer replace for targeted edits"));
    }
}
