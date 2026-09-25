//! 哈希锚点编辑的纯核心。
//!
//! 这一层完全不碰文件系统、不碰宿主：给它内容与参数，它给出新内容或一个带
//! `[E_*]` 标记的错误。因此可以被完整单元测试覆盖，也是移植时唯一需要逐行
//! 对齐原版的部分。
//!
//! 上游：<https://github.com/sleepinginsummer/dsh-hashline-edit-pro>（MIT），
//! 它是 <https://github.com/RimuruW/pi-hashline-edit>（MIT）的 DSH 移植。

pub mod anchor;
pub mod apply;
pub mod diff;
pub mod error;
pub mod hash;
pub mod lines;
pub mod request;
pub mod xxh32;

pub use error::{EditError, ErrorCode};
pub use hash::{HASH_LEN, HASH_SEP};
pub use request::{EditRequest, HashRef, RawEdit};
