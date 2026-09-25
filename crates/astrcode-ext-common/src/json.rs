//! JSON 编解码，以 simd-json 为主路径。
//!
//! 为什么用 simd-json：它用 SIMD 指令做结构定位与转义扫描，并带运行时 CPU
//! 特性探测，无 AVX2 时自动降级。对插件自己拥有的字节缓冲（会话状态账本、
//! 本地配置文件）这条路径，它比 `serde_json` 更快。
//!
//! simd-json 要求输入**可变**（解析时就地反转义、就地解析数字），因此接口分成两类：
//! - `parse_owned` / `value_owned`：接收 `Vec<u8>` 所有权，零拷贝原地解析。
//! - `parse` / `parse_str` / `value`：接收借用，内部复制一份再解析。
//!
//! 宿主回包**不经过本模块**：worker SDK 已经把 S5R 载荷解析成
//! `serde_json::Value` 交给 handler，插件拿到的是 DOM，不是字节。

use serde::Serialize;
use serde::de::DeserializeOwned;

/// 拥有所有权的 JSON 值。
pub type Value = simd_json::OwnedValue;

/// simd-json 的 `json!` 宏，供扩展直接构造 JSON 值。
pub use simd_json::json;
/// 标量 / 数组 / 对象读取与可变对象访问所需的一整套 trait。
pub use simd_json::prelude::{
    MutableObject, ObjectMut, ValueAsArray, ValueAsMutObject, ValueAsObject, ValueAsScalar,
    ValueObjectAccess, Writable,
};

/// 借用输入的 JSON 值（零拷贝读取字符串与数组）。
pub type Borrowed<'a> = simd_json::BorrowedValue<'a>;

/// JSON 解析错误。
#[derive(Debug, thiserror::Error)]
#[error("JSON 解析失败：{0}")]
pub struct ParseError(#[from] pub simd_json::Error);

/// JSON 序列化错误。
#[derive(Debug, thiserror::Error)]
#[error("JSON 序列化失败：{0}")]
pub struct SerializeError(pub simd_json::Error);

/// 原地解析拥有所有权的字节缓冲。
pub fn parse_owned<T: DeserializeOwned>(mut bytes: Vec<u8>) -> Result<T, ParseError> {
    Ok(simd_json::serde::from_slice(&mut bytes)?)
}

/// 解析借用的字节切片（内部复制一份）。
pub fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ParseError> {
    parse_owned(bytes.to_vec())
}

/// 解析字符串（内部复制一份）。
pub fn parse_str<T: DeserializeOwned>(s: &str) -> Result<T, ParseError> {
    parse(s.as_bytes())
}

/// 原地解析为通用 JSON 值。
pub fn value_owned(mut bytes: Vec<u8>) -> Result<Value, ParseError> {
    Ok(simd_json::to_owned_value(&mut bytes)?)
}

/// 解析为通用 JSON 值（内部复制一份）。
pub fn value(bytes: &[u8]) -> Result<Value, ParseError> {
    value_owned(bytes.to_vec())
}

/// 原地解析为借用输入的 JSON 值，零拷贝。
pub fn borrowed_value(bytes: &mut [u8]) -> Result<Borrowed<'_>, ParseError> {
    Ok(simd_json::to_borrowed_value(bytes)?)
}

/// 读取对象字段的字符串值。
pub fn get_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

/// 读取对象字段。
pub fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key)
}

/// 序列化为紧凑 JSON。
pub fn to_string<T: Serialize>(value: &T) -> Result<String, SerializeError> {
    simd_json::to_string(value).map_err(SerializeError)
}

/// 序列化为字节。
pub fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, SerializeError> {
    simd_json::to_vec(value).map_err(SerializeError)
}

/// 序列化为带缩进的 JSON。落盘给人看的配置文件走这条。
pub fn to_string_pretty<T: Serialize>(value: &T) -> Result<String, SerializeError> {
    simd_json::to_string_pretty(value).map_err(SerializeError)
}

/// 序列化为带缩进的字节。
pub fn to_vec_pretty<T: Serialize>(value: &T) -> Result<Vec<u8>, SerializeError> {
    simd_json::to_vec_pretty(value).map_err(SerializeError)
}

/// 把实现了 `Serialize` 的值转换为通用 JSON 值。
pub fn to_value<T: Serialize>(value: T) -> Result<Value, SerializeError> {
    simd_json::serde::to_owned_value(value).map_err(SerializeError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
    struct Ledger {
        schema: u32,
        cursor: String,
    }

    #[test]
    fn parse_str_reads_typed_struct() {
        let got: Ledger = parse_str(r#"{"schema":1,"cursor":"42"}"#).expect("解析失败");
        assert_eq!(
            got,
            Ledger {
                schema: 1,
                cursor: "42".into()
            }
        );
    }

    #[test]
    fn parse_owned_takes_ownership() {
        let bytes = br#"{"schema":1,"cursor":"7"}"#.to_vec();
        let got: Ledger = parse_owned(bytes).expect("解析失败");
        assert_eq!(got.cursor, "7");
    }

    #[test]
    fn parse_reports_error_on_invalid_json() {
        let err = parse::<Ledger>(b"{oops}").expect_err("应报错");
        assert!(err.to_string().contains("JSON 解析失败"));
    }

    #[test]
    fn round_trip_preserves_fields() {
        let ledger = Ledger {
            schema: 1,
            cursor: "99".into(),
        };
        let text = to_string(&ledger).expect("序列化失败");
        let back: Ledger = parse_str(&text).expect("解析失败");
        assert_eq!(back, ledger);
    }

    #[test]
    fn value_parses_object_and_reads_fields() {
        let v = value(br#"{"cursor":"12"}"#).expect("解析失败");
        assert_eq!(get_str(&v, "cursor"), Some("12"));
        assert_eq!(get_str(&v, "missing"), None);
    }

    #[test]
    fn get_str_returns_none_for_non_string() {
        let v = value(br#"{"cursor":12}"#).expect("解析失败");
        assert_eq!(get_str(&v, "cursor"), None);
    }

    #[test]
    fn to_vec_produces_parseable_bytes() {
        let ledger = Ledger {
            schema: 2,
            cursor: "3".into(),
        };
        let bytes = to_vec(&ledger).expect("序列化失败");
        let back: Ledger = parse_owned(bytes).expect("解析失败");
        assert_eq!(back, ledger);
    }

    #[test]
    fn pretty_output_is_indented_and_parseable() {
        let ledger = Ledger {
            schema: 3,
            cursor: "5".into(),
        };
        let text = to_string_pretty(&ledger).expect("序列化失败");
        assert!(text.contains('\n'), "pretty 输出应当换行：{text}");
        assert_eq!(parse_str::<Ledger>(&text).expect("解析失败"), ledger);

        let bytes = to_vec_pretty(&ledger).expect("序列化失败");
        assert_eq!(parse_owned::<Ledger>(bytes).expect("解析失败"), ledger);
    }
}
