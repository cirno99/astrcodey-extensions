//! `pre_tool_use` 工具守卫。
//!
//! 对应 phi-deepseek-enhanced 的 Eternal Minimal 运行时守卫，但**语义相反**：phi 用白名单
//! （只允许 `coreTools` + `transportTools` 直呼，其余全拦），在 AstrCode 里那会把别的扩展
//! 注册的工具一起拦掉，模型没有任何自救手段——只能靠用户手改配置恢复，是最坏的死锁。
//! 这里改成黑名单：只拦配置里显式列出的工具，其余一律放行，误伤面等于零。
//!
//! 拦截如果只说「不允许」，模型往往会原样重试；给出可直接照做的替代用法，才能把
//! 「试 → 被拒 → 重试」压成一次。

/// 被拦工具 → 替代用法。
///
/// 工具名取自 AstrCode 宿主内建工具与随本工作区分发的扩展工具。
fn substitute_hint(tool: &str) -> &'static str {
    match tool {
        "edit" => {
            "改用 `hashline_read` 读文件（返回 HASH│content 行），再用 `replace` 按锚点做定点\
             编辑：`remove_from` / `remove_to` 只填裸的 3 字符锚点，不要带行号或行内容。"
        }
        "patch" => {
            "改用 `replace` 分段修改；未改动行的锚点在多次 `replace` 之间保持有效，\
             因此连续编辑无需重新读取。"
        }
        "read" => "改用 `hashline_read`（带锚点，便于后续 `replace`）或 `read_tool_result`。",
        "write" => {
            "新建文件仍需要 `write`（`replace` 不能创建文件）；只是修改已有文件时改用 `replace`。"
        }
        "grep" => "改用 `shell` 执行 `rg -n <pattern>`。",
        "glob" => "改用 `shell` 执行 `rg --files`。",
        "retrieve" => "改用 `read_tool_result` 直接取回被换出的结果。",
        "agent" | "agent_spawn" | "agent_list" | "agent_wait" | "agent_cancel" => {
            "本机已在配置里禁用子代理，请用 `shell` 与工作区工具直接完成。"
        }
        _ => "请改用本会话允许的工具完成该操作。",
    }
}

/// 判定一次工具调用是否该被拦下；返回拦截原因。
///
/// 白名单为空时**不拦任何调用**。`blocked_tools` 由配置显式给出，空列表就是「守卫开着但
/// 不拦任何工具」，这是合法配置而非错误。
pub fn block_reason(tool_name: &str, blocked: &[String]) -> Option<String> {
    if !blocked.iter().any(|name| name == tool_name) {
        return None;
    }
    Some(format!(
        "工具守卫阻止对 `{tool_name}` 的直接调用（本会话被拦工具：{}）。{}\n\
         放行方式：`/weneed guard off` 关闭守卫，或 `/weneed block remove {tool_name}` \
         只放行这一个工具。",
        blocked.join(", "),
        substitute_hint(tool_name)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(tools: &[&str]) -> Vec<String> {
        tools.iter().map(|tool| (*tool).to_owned()).collect()
    }

    #[test]
    fn a_listed_tool_is_blocked() {
        let reason = block_reason("edit", &blocked(&["edit"])).expect("edit 应当被拦");
        assert!(reason.contains("`edit`"), "{reason}");
        assert!(reason.contains("`replace`"), "{reason}");
        assert!(reason.contains("`hashline_read`"), "{reason}");
    }

    #[test]
    fn an_unlisted_tool_passes_through() {
        assert!(block_reason("write", &blocked(&["edit"])).is_none());
        assert!(block_reason("shell", &blocked(&["edit"])).is_none());
    }

    /// 守卫开着但名单为空时不拦任何工具。
    #[test]
    fn an_empty_blocklist_blocks_nothing() {
        assert!(block_reason("edit", &[]).is_none());
    }

    /// 模型必须能自己发现放行方式，否则会陷入「试 → 被拒 → 重试」。
    #[test]
    fn the_reason_tells_the_model_how_to_get_unblocked() {
        let reason = block_reason("edit", &blocked(&["edit"])).unwrap();
        assert!(reason.contains("/weneed guard off"), "{reason}");
        assert!(reason.contains("/weneed block remove edit"), "{reason}");
    }

    #[test]
    fn the_reason_lists_the_whole_blocklist() {
        let reason = block_reason("edit", &blocked(&["edit", "patch"])).unwrap();
        assert!(reason.contains("edit, patch"), "{reason}");
    }

    /// 黑名单里的未知工具名也要给出兜底提示，而不是留一段空话。
    #[test]
    fn an_unknown_tool_falls_back_to_a_generic_hint() {
        let reason = block_reason("whatever", &blocked(&["whatever"])).unwrap();
        assert!(reason.contains("本会话允许的工具"), "{reason}");
    }

    /// 名字必须精确相等：`edit` 被拦不该连带拦掉 `edit_notes`。
    #[test]
    fn matching_is_exact() {
        assert!(block_reason("edit_notes", &blocked(&["edit"])).is_none());
        assert!(block_reason("myedit", &blocked(&["edit"])).is_none());
    }
}
