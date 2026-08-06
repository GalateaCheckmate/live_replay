use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

#[tokio::test]
async fn replay_migrations_and_delete_guards_work() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let database = directory.path().join("replay.sqlite3");
    let url = format!("sqlite://{}", database.display());
    let options = SqliteConnectOptions::from_str(&url)
        .expect("parse sqlite URL")
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open sqlite database");

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("all migrations must run on a fresh database");

    sqlx::query(
        "INSERT INTO uploadstreamers (id, template_name, tags) VALUES (1, 'test-template', '[]')",
    )
    .execute(&pool)
    .await
    .expect("insert upload template");
    sqlx::query(
        "INSERT INTO livestreamers (id, url, remark, upload_streamers_id) \
         VALUES (1, 'https://example.test/live/1', 'test', 1)",
    )
    .execute(&pool)
    .await
    .expect("insert monitored streamer");
    sqlx::query(
        "INSERT INTO streamerinfo (id, name, url, title, date, live_cover_path) \
         VALUES (1, 'streamer', 'https://example.test/live/1', 'title', \
                 CURRENT_TIMESTAMP, '')",
    )
    .execute(&pool)
    .await
    .expect("insert streamer info");
    sqlx::query(
        "INSERT INTO live_sessions \
         (id, live_streamer_id, source_streamer_info_id, streamer_name, streamer_url, \
          live_title, started_at, status) \
         VALUES (1, 1, 1, 'streamer', 'https://example.test/live/1', \
                 'title', CURRENT_TIMESTAMP, 'recording')",
    )
    .execute(&pool)
    .await
    .expect("insert active replay session");

    let streamer_delete = sqlx::query("DELETE FROM livestreamers WHERE id = 1")
        .execute(&pool)
        .await
        .expect_err("active replay session must block streamer deletion");
    assert!(
        streamer_delete
            .to_string()
            .contains("Live Replay 上传任务"),
        "unexpected delete error: {streamer_delete}"
    );

    let template_delete = sqlx::query("DELETE FROM uploadstreamers WHERE id = 1")
        .execute(&pool)
        .await
        .expect_err("referenced upload template must not be deleted");
    assert!(
        template_delete.to_string().contains("投稿模板仍被主播使用"),
        "unexpected delete error: {template_delete}"
    );

    sqlx::query("UPDATE live_sessions SET status = 'complete' WHERE id = 1")
        .execute(&pool)
        .await
        .expect("complete replay session");
    sqlx::query("DELETE FROM livestreamers WHERE id = 1")
        .execute(&pool)
        .await
        .expect("completed replay permits monitored streamer deletion");
    sqlx::query("DELETE FROM uploadstreamers WHERE id = 1")
        .execute(&pool)
        .await
        .expect("unreferenced template can be deleted");
}
