//! 마스터 패스워드 기반 로컬 암호화.
//! 파일 포맷: MAGIC(4) | salt(16) | nonce(12) | ciphertext
//! KDF: Argon2id, AEAD: AES-256-GCM

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::Argon2;
use rand::RngCore;

const MAGIC: &[u8; 4] = b"HMV1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("키 유도 실패: {e}"))?;
    Ok(key)
}

pub fn encrypt(password: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    let mut rng = rand::rngs::OsRng;
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce_bytes);

    let key = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("암호화 실패: {e}"))?;

    let mut out = Vec::with_capacity(4 + SALT_LEN + NONCE_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt(password: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let header = 4 + SALT_LEN + NONCE_LEN;
    if data.len() < header || &data[0..4] != MAGIC {
        return Err("저장 파일 형식이 올바르지 않습니다".into());
    }
    let salt = &data[4..4 + SALT_LEN];
    let nonce_bytes = &data[4 + SALT_LEN..header];
    let ct = &data[header..];

    let key = derive_key(password, salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ct)
        .map_err(|_| "마스터 패스워드가 틀렸거나 파일이 손상되었습니다".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_ok() {
        let ct = encrypt("master-pw", b"hello secret").unwrap();
        assert_eq!(decrypt("master-pw", &ct).unwrap(), b"hello secret");
    }

    #[test]
    fn wrong_password_fails() {
        let ct = encrypt("right", b"data").unwrap();
        assert!(decrypt("wrong", &ct).is_err());
    }

    #[test]
    fn corrupt_header_fails() {
        assert!(decrypt("x", b"not-a-valid-file").is_err());
    }
}
