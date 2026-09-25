#!/usr/bin/env sh
# 构建并安装本工作区的扩展到宿主的全局扩展目录。
#
# 宿主在两个位置发现磁盘扩展（见 astrcode-extensions::loader::discover_all）：
#   - <home>/.astrcode/extensions/           全局
#   - <working_dir>/.astrcode/extensions/    项目级（优先级更高）
# 本脚本装到全局目录。home 与宿主一致：ASTRCODE_HOME_DIR 覆盖系统 home。
#
# 用法：install.sh [扩展 ID ...]    不带参数时安装全部。
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DEST_ROOT="${ASTRCODE_HOME_DIR:-$HOME}/.astrcode/extensions"

# 扩展 ID、cargo 包名、产物二进制名是三个不同的东西，不要混用：
#   extension_id = astrcode-<name>
#   package      = astrcode-ext-<name>   （= 产物二进制名）
# 后两者因此可以从 ID 推导，不需要维护一张会漂移的映射表。
package_of() {
    printf 'astrcode-ext-%s\n' "${1#astrcode-}"
}

# 从 `extension.json` 里读出 `extension_id`。
manifest_id() {
    sed -n 's/.*"extension_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$1"
}

# 扩展 ID 必须同时等于 `extension.json` 的 `extension_id` 与源码里 `EXTENSION_ID`
# 常量的值——宿主按 ID 定位扩展，扩展自己按 ID 定位数据目录，两者不一致会得到
# 「装了但读不到自己的配置」这类最难查的故障。这里把三者钉死，不一致就退出。
verify_identity() {
    extension_id=$1
    package=$2
    manifest="$ROOT/crates/$package/extension.json"

    if [ ! -f "$manifest" ]; then
        echo "找不到 $manifest——扩展 ID「$extension_id」是否拼错？" >&2
        exit 1
    fi

    declared=$(manifest_id "$manifest")
    if [ "$declared" != "$extension_id" ]; then
        echo "不一致：$manifest 声明的是「$declared」，脚本用的是「$extension_id」" >&2
        exit 1
    fi

    # 常量声明位置各 crate 不同（`lib.rs` / `command.rs` / `worker.rs`），因此整棵
    # `src/` 一起找，而不是钉死某个文件。
    registered=$(grep -rho 'EXTENSION_ID: &str = "[^"]*"' "$ROOT/crates/$package/src" | head -1)
    # 先摘结尾的引号，再摘前缀：顺序反了会把整串吃空，断言就会把每个扩展都判成不一致。
    registered=${registered%\"}
    registered=${registered##*\"}
    if [ "$registered" != "$extension_id" ]; then
        echo "不一致：$package 源码里注册的是「$registered」，extension.json 声明的是「$declared」" >&2
        exit 1
    fi
}

install_one() {
    extension_id=$1
    package=$(package_of "$extension_id")
    verify_identity "$extension_id" "$package"

    dest="$DEST_ROOT/$extension_id"
    cargo build --release --manifest-path "$ROOT/Cargo.toml" -p "$package"

    mkdir -p "$dest"
    install -m 0755 "$ROOT/target/release/$package" "$dest/$package"
    install -m 0644 "$ROOT/crates/$package/extension.json" "$dest/extension.json"

    echo "已安装 $extension_id -> $dest"
}

if [ "$#" -eq 0 ]; then
    # 默认安装全部：直接从各 crate 的 `extension.json` 读 ID，因此新增扩展只要建好
    # manifest 就会被带上，不必回来改这份列表。
    for manifest in "$ROOT"/crates/*/extension.json; do
        [ -f "$manifest" ] || continue
        set -- "$@" "$(manifest_id "$manifest")"
    done
fi

for wanted in "$@"; do
    install_one "$wanted"
done

echo "重启 astrcode 后生效。"
