use crate::server::api::bilibili_endpoints::{
    archive_pre_endpoint, get_myinfo_endpoint, get_proxy_endpoint,
};
use crate::server::api::endpoints::{
    add_upload_streamer_endpoint, add_user_endpoint, delete_streamers_endpoint,
    delete_template_endpoint, delete_user_endpoint, get_configuration, get_qrcode, get_status,
    get_streamer_info, get_streamer_info_files, get_streamers_endpoint,
    get_upload_streamer_endpoint, get_upload_streamers_endpoint, get_users_endpoint, get_videos,
    login_by_qrcode, pause_streamers_endpoint, post_streamers_endpoint, post_uploads,
    put_configuration, put_streamers_endpoint,
};
use crate::server::api::replay_endpoints::{
    get_replay_jobs, get_replay_sessions, retry_replay_job,
};
use crate::server::infrastructure::service_register::ServiceRegister;
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use tower::ServiceExt;
use tower_http::services::ServeFile;
/// 创建应用程序路由
pub fn router(service_register: ServiceRegister) -> Router<()> {
    Router::new()
        // 主播管理相关路由
        .route(
            "/v1/streamers",
            get(get_streamers_endpoint)
                .post(post_streamers_endpoint)
                .put(put_streamers_endpoint),
        )
        .route("/v1/streamers/{id}", delete(delete_streamers_endpoint))
        .route("/v1/streamers/{id}/pause", put(pause_streamers_endpoint))
        // 配置管理路由
        .route(
            "/v1/configuration",
            get(get_configuration).put(put_configuration),
        )
        // 主播信息路由
        .route("/v1/streamer-info", get(get_streamer_info))
        .route("/v1/streamer-info/files/{id}", get(get_streamer_info_files))
        // 上传模板管理路由
        .route("/v1/upload/streamers", get(get_upload_streamers_endpoint))
        .route(
            "/v1/upload/streamers/{id}",
            delete(delete_template_endpoint).get(get_upload_streamer_endpoint),
        )
        .route("/v1/upload/streamers", post(add_upload_streamer_endpoint))
        // Live Replay 场次和持久化上传队列
        .route("/v1/replay/sessions", get(get_replay_sessions))
        .route("/v1/replay/jobs", get(get_replay_jobs))
        .route("/v1/replay/jobs/{id}/retry", post(retry_replay_job))
        // 用户管理路由
        .route("/v1/users", get(get_users_endpoint).post(add_user_endpoint))
        .route("/v1/users/{id}", delete(delete_user_endpoint))
        // B站API代理路由
        .route("/bili/archive/pre", get(archive_pre_endpoint))
        .route("/bili/space/myinfo", get(get_myinfo_endpoint))
        .route("/bili/proxy", get(get_proxy_endpoint))
        // 认证相关路由
        .route("/v1/get_qrcode", get(get_qrcode))
        .route("/v1/login_by_qrcode", post(login_by_qrcode))
        // 视频文件管理路由
        .route("/v1/videos", get(get_videos))
        .route("/v1/status", get(get_status))
        .route("/v1/uploads", post(post_uploads))
        .route_service("/static/{path}", get(using_serve_file_from_a_route))
        .with_state(service_register)
}

async fn using_serve_file_from_a_route(
    axum::extract::Path(path): axum::extract::Path<String>,
    request: Request<Body>,
) -> impl IntoResponse {
    let serve_file = ServeFile::new(path);
    serve_file.oneshot(request).await
}
