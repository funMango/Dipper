use thiserror::Error;

/// Errors returned by the Dipper library.
#[derive(Debug, Error)]
pub enum DipperError {
    /// The command exists but is not implemented yet.
    #[error("{0}: not implemented yet")]
    NotImplemented(&'static str),
}

impl DipperError {
    /// 종료코드 규약: 0 성공, 1 설정·실행 오류, 2 사용법 오류·미구현.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotImplemented(_) => 2,
        }
    }
}
