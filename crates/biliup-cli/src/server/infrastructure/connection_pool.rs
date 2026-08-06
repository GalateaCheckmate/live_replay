use crate::server::errors::{AppError, AppResult};
use chrono::Utc;
use error_stack::ResultExt;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Pool, Sqlite};
use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::info;

pub type ConnectionPool = Pool<Sqlite>;

static STARTUP_CUTOFF: OnceLock<String> = OnceLock::new();

/// 本进程开始连接数据库的时间。恢复器只把这个时间之前的活动场次视为崩溃遗留。
pub fn startup_cutoff() -> &'static str {
    STARTUP_CUTOFF
        .get_or_init(|| Utc::now().to_rfc3339())
        .as_str()
}

pub struct ConnectionManager;

impl ConnectionManager {
    pub async fn new_pool(path: &str) -> AppResult<ConnectionPool> {
        let _ = startup_cutoff();

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
