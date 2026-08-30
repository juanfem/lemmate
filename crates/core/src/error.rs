use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("crdt update could not be decoded or applied: {0}")]
    Crdt(String),
    #[error("sync frame malformed: {0}")]
    Frame(&'static str),
    #[error("markdown: {0}")]
    Markdown(String),
    #[error("front matter: {0}")]
    FrontMatter(#[from] serde_yaml_ng::Error),
    #[error("invalid id: {0}")]
    Id(String),
    #[error("watcher: {0}")]
    Watcher(#[from] notify::Error),
    #[error("path escapes vault root: {0}")]
    PathEscape(String),
    #[error("sync: {0}")]
    Sync(String),
    #[error("import: {0}")]
    Import(String),
    #[error("export: {0}")]
    Export(String),
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
