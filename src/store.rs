//! 저장소 파일 입출력 및 경로 관리.

use crate::crypto;
use crate::model::Store;
use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
}

/// 암호화된 저장소 파일 경로 (~/.config/hostmover/store.enc)
pub fn store_path() -> PathBuf {
    home_dir().join(".config").join("hostmover").join("store.enc")
}

/// 백업 파일 보관 루트 (~/.local/share/hostmover/backups)
pub fn backups_root() -> PathBuf {
    home_dir().join(".local").join("share").join("hostmover").join("backups")
}

pub fn exists() -> bool {
    store_path().exists()
}

pub fn load(password: &str) -> Result<Store, String> {
    let path = store_path();
    let data = std::fs::read(&path).map_err(|e| format!("파일 읽기 실패: {e}"))?;
    let plain = crypto::decrypt(password, &data)?;
    serde_json::from_slice(&plain).map_err(|e| format!("데이터 파싱 실패: {e}"))
}

pub fn save(password: &str, store: &Store) -> Result<(), String> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("디렉터리 생성 실패: {e}"))?;
    }
    let plain = serde_json::to_vec_pretty(store).map_err(|e| format!("직렬화 실패: {e}"))?;
    let enc = crypto::encrypt(password, &plain)?;
    // 원자적 저장: 임시파일 후 rename
    let tmp = path.with_extension("enc.tmp");
    std::fs::write(&tmp, &enc).map_err(|e| format!("임시파일 쓰기 실패: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("저장 실패: {e}"))?;
    Ok(())
}

/// 파일 이름으로 안전한 문자열로 변환
pub fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() { "_".into() } else { trimmed }
}
