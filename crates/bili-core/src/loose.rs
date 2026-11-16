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

/// 宽松可选无符号整数：`null` 或非数字占位（如 Lv6 的 `next_exp: "--"`）回退 `None`。
pub fn opt_u64_any<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        Null,
        Num(f64),
        Text(String),
    }
    match Option::<V>::deserialize(d)? {
        None | Some(V::Null) => Ok(None),
        Some(V::Num(f)) if f.is_finite() && f >= 0.0 => Ok(Some(f.round() as u64)),
        Some(V::Num(_)) => Ok(None),
        Some(V::Text(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            match trimmed.parse::<f64>() {
                Ok(f) if f.is_finite() && f >= 0.0 => Ok(Some(f.round() as u64)),
                _ => Ok(None),
            }
        }
    }
}

/// 宽松浮点：`null` 兜底为 `0.0`。
pub fn f64_any_or_zero<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        Null,
        Num(f64),
        Text(String),
    }
    match Option::<V>::deserialize(d)? {
        None | Some(V::Null) => Ok(0.0),
        Some(V::Num(f)) if f.is_finite() => Ok(f),
        Some(V::Num(_)) => Ok(0.0),
        Some(V::Text(s)) => Ok(s.trim().parse::<f64>().unwrap_or(0.0)),
    }
}

/// 宽松无符号整数：`null` 或非法值兜底为 `0`。
pub fn u64_any_or_zero<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    Ok(opt_u64_any(d)?.unwrap_or(0))
}

/// 宽松有符号整数：`null` 或非法值兜底为 `0`。
pub fn i64_any_or_zero<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<i64, D::Error> {
    let f = f64_any_or_zero(d)?;
    Ok(if f.is_finite() { f.round() as i64 } else { 0 })
}
