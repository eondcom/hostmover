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

/// 백업 파일 생성 — 마스터 비밀번호로 암호화한 .hmbak 파일. 생성 경로 반환.
pub fn export_backup(password: &str, store: &Store) -> Result<PathBuf, String> {
    let dir = backups_root();
    std::fs::create_dir_all(&dir).map_err(|e| format!("백업 폴더 생성 실패: {e}"))?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("hostmover-backup-{secs}.hmbak"));
    let plain = serde_json::to_vec_pretty(store).map_err(|e| format!("직렬화 실패: {e}"))?;
    let enc = crypto::encrypt(password, &plain)?;
    std::fs::write(&path, &enc).map_err(|e| format!("백업 쓰기 실패: {e}"))?;
    Ok(path)
}

/// 백업 파일 복원 — .hmbak(암호화) 우선, 실패 시 평문 JSON 으로도 시도.
pub fn import_backup(password: &str, path: &std::path::Path) -> Result<Store, String> {
    let data = std::fs::read(path).map_err(|e| format!("파일 읽기 실패: {e}"))?;
    if let Ok(plain) = crypto::decrypt(password, &data) {
        return serde_json::from_slice(&plain).map_err(|e| format!("데이터 파싱 실패: {e}"));
    }
    // 평문 JSON 백업도 허용
    serde_json::from_slice(&data).map_err(|_| "복원 실패: 비밀번호가 다르거나 손상된 파일".to_string())
}

/// 백업 폴더의 .hmbak 파일 목록 (최신순) → (경로, 파일명)
pub fn list_backups() -> Vec<(PathBuf, String)> {
    let mut v: Vec<(PathBuf, String, std::time::SystemTime)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(backups_root()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("hmbak") {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                let mtime = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                v.push((p, name, mtime));
            }
        }
    }
    v.sort_by(|a, b| b.2.cmp(&a.2));
    v.into_iter().map(|(p, n, _)| (p, n)).collect()
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
