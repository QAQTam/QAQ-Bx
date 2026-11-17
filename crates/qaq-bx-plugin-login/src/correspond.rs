//! correspond path 原生计算（替代官方 wasm）。
//!
//! wasm 逆向结论（docs/bilibili-login-research.md）：
//! - 算法：RSA-1024 + OAEP(SHA256)，公钥硬编码于 wasm data section
//! - 输入：hex(plaintext) 字符串 → wasm 内部 hex::decode → 明文字节 → OAEP 加密
//! - 输出：128 字节密文的 hex（256 字符），即 correspond path
//! - OAEP 有随机填充，同一明文每次密文不同（服务器用私钥解密）
//! - 官方 wasm producers: Rust / rustc 1.60.0 / wasm-bindgen 0.2.58，
//!   panic 路径含 rsa crate git 版 (oaep.rs/internals.rs/key.rs) + num-bigint-dig 0.6.0 + hex 0.4.0

use rsa::{rand_core::OsRng, BigUint, Oaep, RsaPublicKey};
use sha2::Sha256;

use qaq_bx_core::error::{BiliError, Result};

/// RSA-1024 公钥模数（从官方 wasm_rsa_encrypt_bg.wasm data section 提取，256 hex）
pub const PUBLIC_KEY_MODULUS_HEX: &str = "cb81dd8e02470656da04dd38544446e2a3412051cfe9adc6a330a5ef90228509684960970b91c3360ca29c49e1690ff8fa068cb9dfc6179d1e9585cb9424e847db1ef59f33e37dd4dca8ccfb7631ee9b4a92640d00c8204300152a0ab7cd802889d3445aec69918fe6022b534912e7b095be3424dad1ba81145e969b533181f1";
/// 公钥指数
pub const PUBLIC_KEY_EXPONENT: u64 = 65537;

fn public_key() -> Result<RsaPublicKey> {
    let n_bytes = hex::decode(PUBLIC_KEY_MODULUS_HEX)
        .map_err(|e| BiliError::InvalidData(format!("公钥模数解码失败: {e}")))?;
    let n = BigUint::from_bytes_be(&n_bytes);
    let e = BigUint::from(PUBLIC_KEY_EXPONENT);
    RsaPublicKey::new(n, e).map_err(|e| BiliError::InvalidData(format!("公钥构造失败: {e}")))
}

/// 加密明文字节 → correspond path（256 hex 字符）。
/// 与官方 wasm 的 `wasmInit.encrypt({data: hex(plaintext), digest: "SHA256"})` 等价。
pub fn encrypt_correspond(plaintext: &[u8]) -> Result<String> {
    let key = public_key()?;
    let mut rng = OsRng;
    let ct = key
        .encrypt(&mut rng, Oaep::new::<Sha256>(), plaintext)
        .map_err(|e| BiliError::InvalidData(format!("OAEP 加密失败: {e}")))?;
    Ok(hex::encode(ct))
}

/// 便捷封装：与 miniLogin 的 getEncryptCode 一致 —— 先 hex 编码再加密。
/// `plaintext_kind`: "set"（登录后存 token）或 "refresh"（检测到需刷新）。
pub fn correspond_path(plaintext_kind: &str, timestamp_ms: i64) -> Result<String> {
    let plain = format!("{plaintext_kind}_{timestamp_ms}");
    let data_hex = hex::encode(plain.as_bytes());
    // wasm 收到的是 hex 字符串，内部 hex::decode 后作为明文字节
    let plain_bytes = hex::decode(&data_hex)
        .map_err(|e| BiliError::InvalidData(format!("hex 解码失败: {e}")))?;
    encrypt_correspond(&plain_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 输出结构与官方 wasm 一致：256 hex；OAEP 随机化 → 两次不同。
    #[test]
    fn encrypt_shape_matches_wasm() {
        let ts = 1794751953660_i64;
        let data_hex = hex::encode(format!("set_{ts}"));
        let p1 = encrypt_correspond(data_hex.as_bytes()).unwrap();
        let p2 = encrypt_correspond(data_hex.as_bytes()).unwrap();
        assert_eq!(p1.len(), 256);
        assert_eq!(p2.len(), 256);
        assert_ne!(p1, p2, "OAEP 随机填充下两次密文应不同");
        // 明文解码后与输入一致（自证 hex 往返）
        assert_eq!(hex::decode(&data_hex).unwrap(), format!("set_{ts}").as_bytes());
    }

    /// correspond_path 便捷封装的输出形状。
    #[test]
    fn correspond_path_shape() {
        let p = correspond_path("refresh", 1794751953660).unwrap();
        assert_eq!(p.len(), 256);
        assert!(p.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
