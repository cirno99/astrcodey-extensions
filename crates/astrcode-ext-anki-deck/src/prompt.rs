//! 注入 system prompt 的引导语。
//!
//! 走 `prompt_build` 钩子，宿主把贡献映射成 `ExtensionSection::PlatformInstructions`，
//! 落在 system prompt 的**静态前缀区**，只在贡献变化时让 provider 前缀缓存失效。
//!
//! 正文是英文（模型可见工件，与 hashline-edit 的 GUIDANCE 同一待遇）；职责边界写在
//! 正文里：语义活（挑卡、组织卡面）归模型，打包归工具。**改动必须同步 `AGENTS.md`
//! 的对应小节，两处不一致即为 bug。**

/// 注入到 system prompt 的引导语。
pub const GUIDANCE: &str = "Anki deck packaging is available: anki_write_apkg turns a JSON deck spec into a ready-to-import .apkg file. Gather flashcards from the markdown notes yourself (read the vault with your file tools), then call the tool once per top-level deck. Spec: output is the target .apkg path; deck.name uses :: for subdecks (e.g. \"Rust::Ownership\"); deck.cards carry front/back (HTML allowed), cloze:true plus {{c1::...}} markers in the front for cloze cards, whitespace-free tags, and id — a stable identity like \"vault:notes/rust.md#ownership\" derived from the source path and heading, which fixes the note GUID so regenerating a vault and re-importing updates cards instead of duplicating them; deck.media lists referenced image/audio paths, referenced in card HTML by bare file name. The tool validates the spec and returns [E_*] errors with corrective hints; report the output path and card count when done.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_names_the_tool() {
        assert!(GUIDANCE.contains("anki_write_apkg"));
    }

    #[test]
    fn guidance_states_the_guid_stability_contract() {
        // GUID 稳定是「重复生成 = 更新」的前提，引导必须把这个要求传达给模型。
        assert!(GUIDANCE.contains("stable identity"));
        assert!(GUIDANCE.contains("updates cards instead of duplicating"));
    }

    #[test]
    fn guidance_documents_the_subdeck_and_cloze_syntax() {
        assert!(GUIDANCE.contains(":: for subdecks"));
        assert!(GUIDANCE.contains("{{c1::...}}"));
    }

    #[test]
    fn guidance_points_media_references_at_bare_file_names() {
        assert!(GUIDANCE.contains("bare file name"));
    }
}
