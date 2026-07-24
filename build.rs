//! 빌드 시점의 git 커밋/날짜를 실행파일에 새겨 넣는다.
//! 앱 첫 화면과 로그 첫 줄에 표시해, 지금 돌고 있는 바이너리가 어느 버전인지
//! (런처가 옛 바이너리를 가리키는 흔한 사고를 포함해) 눈으로 확인할 수 있게 한다.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn main() {
    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "nogit".into());
    // 커밋 시각 = 재현 가능한 빌드 식별자 (빌드 시각과 달리 같은 소스면 항상 같다)
    let date = git(&["log", "-1", "--format=%cd", "--date=format:%Y-%m-%d %H:%M"])
        .unwrap_or_else(|| "unknown".into());
    // 커밋되지 않은 변경이 섞인 빌드는 명시한다
    let dirty = match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(s) if !s.is_empty() => "+수정본",
        _ => "",
    };

    println!("cargo:rustc-env=HM_GIT_HASH={hash}{dirty}");
    println!("cargo:rustc-env=HM_GIT_DATE={date}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
