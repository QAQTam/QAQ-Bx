//! B 站数字字段的宽松解析:响应里数字字段实测存在字符串形态
//! (如 `frame_rate: "25"`),统一"数字或字符串"反序列化。
//!
//! `player.rs` 与 `danmaku.rs` 等共用;契约锚 `docs/bilibili-api-inventory.md`。

use serde::{Deserialize, Deserializer};

pub fn f64_any<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        Num(f64),
        Text(String),
    }
    match V::deserialize(d)? {
        V::Num(f) => Ok(f),
        V::Text(s) => s.trim().parse::<f64>().map_err(serde::de::Error::custom),
    }
}

pub fn i64_any<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<i64, D::Error> {
    Ok(f64_any(d)?.round() as i64)
}

pub fn i32_any<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<i32, D::Error> {
    Ok(i64_any(d)? as i32)
}

pub fn u64_any<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    Ok(f64_any(d)?.round().max(0.0) as u64)
}

pub fn u32_any<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<u32, D::Error> {
    Ok(u64_any(d)? as u32)
}
