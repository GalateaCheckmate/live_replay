use crate::server::api::bilibili_endpoints::{
    archive_pre_endpoint, get_myinfo_endpoint, get_proxy_endpoint,
};
use crate::server::api::endpoints::{
    add_upload_streamer_endpoint, add_user_endpoint, delete_streamers_endpoint,
    delete_template_endpoint, delete_user_endpoint, get_configuration, get_disk_status_endpoint,
    get_qrcode, get_status, get_streamer_info, get_streamer_info_files, get_streamers_endpoint,
    get_upload_streamer_endpoint, get_upload_streamers_endpoint, get_users_endpoint, get_videos,
    login_by_qrcode, pause_streamers_endpoint, post_simple_streamer_endpoint,
    post_streamers_endpoint, post_uploads, put_configuration, put_streamers_endpoint,
    upload_now_streamer_endpoint,
};
use crate::server::api::replay_endpoints::{
    bind_replay_submission, get_replay_jobs, get_replay_sessions, reset_replay_submission,
    retry_replay_job,
};
use crate::server::api::replay_state_endpoints::get_replay_streamer_states;
use crate::server::infrastructure::service_register::ServiceRegister;
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use tower::ServiceExt;
use tower_http::services::ServeFile;

pub fn router(service_register: ServiceRegister) -> Router<()> {
    Router::new()
        .route(
            "/v1/streamers",
            get(get_streamers_endpoint)
                .post(post_streamers_endpoint)
                .put(put_streamers_endpoint),
        )
        .route("/v1/streamers/simple", post(post_simple_streamer_endpoint))
        .route("/v1/disk-status", get(get_disk_status_endpoint))
        .route("/v1/streamers/{id}", delete(delete_streamers_endpoint))
        .route("/v1/streamers/{id}/pause", put(pause_streamers_endpoint))
        .route(
            "/v1/streamers/{id}/upload-now",
            post(upload_now_streamer_endpoint),
        )
        .route(
            "/v1/configuration",
            get(get_configuration).put(put_configuration),
        )
        .route("/v1/streamer-info", get(get_streamer_info))
        .route("/v1/streamer-info/files/{id}", get(get_streamer_info_files))
        .route("/v1/upload/streamers", get(get_upload_streamers_endpoint))
        .route(
            "/v1/upload/streamers/{id}",
            delete(delete_template_endpoint).get(get_upload_streamer_endpoint),
        )
        .route("/v1/upload/streamers", post(add_upload_streamer_endpoint))
        .route("/v1/replay/streamers", get(get_replay_streamer_states))
        .route("/v1/replay/sessions", get(get_replay_sessions))
        .route("/v1/replay/jobs", get(get_replay_jobs))
        .route("/v1/replay/jobs/{id}/retry", post(retry_replay_job))
        .route(
            "/v1/replay/sessions/{id}/bind-submission",
            post(bind_replay_submission),
        )
        .route(
            "/v1/replay/sessions/{id}/reset-submission",
            post(reset_replay_submission),
        )
        .route("/v1/users", get(get_users_endpoint).post(add_user_endpoint))
        .route("/v1/users/{id}", delete(delete_user_endpoint))
        .route("/bili/archive/pre", get(archive_pre_endpoint))
        .route("/bili/space/myinfo", get(get_myinfo_endpoint))
        .route("/bili/proxy", get(get_proxy_endpoint))
        .route("/v1/get_qrcode", get(get_qrcode))
        .route("/v1/login_by_qrcode", post(login_by_qrcode))
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
