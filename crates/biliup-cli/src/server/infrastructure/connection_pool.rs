use crate::server::errors::{AppError, AppResult};
use error_stack::ResultExt;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Pool, Sqlite};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;
use tracing::info;

/// SQLite连接池类型别名
pub type ConnectionPool = Pool<Sqlite>;

/// 连接管理器
/// 负责管理SQLite连接池的创建和配置
pub struct ConnectionManager;

impl ConnectionManager {
    pub async fn new_pool(path: &str) -> AppResult<ConnectionPool> {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent)
                .change_context(AppError::Unknown)
                .attach_with(|| path.to_string())?;
        }

        let db_url = format!("sqlite://{path}");
        let options = SqliteConnectOptions::from_str(&db_url)
            .change_context(AppError::Unknown)?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(30));

        // WAL 允许录制分段登记与 Web 查询并发；busy_timeout 避免短暂写锁导致分段丢失。
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(options)
            .await
            .change_context(AppError::Custom(
                "error while initializing the database connection pool".to_string(),
            ))?;

        info!("migrations enabled, running...");
        sqlx::migrate!()
            .run(&pool)
            .await
            .change_context(AppError::Custom(
                "error while running database migrations".to_string(),
            ))?;

        Ok(pool)
    }
}
