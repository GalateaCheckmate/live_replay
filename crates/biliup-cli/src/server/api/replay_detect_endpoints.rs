use crate::server::core::live::live_request;
use crate::server::infrastructure::models::live_streamer::LiveStreamer;
use crate::server::infrastructure::service_register::ServiceRegister;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use biliup::downloader::live::{LiveStatus, builtin_plugins};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct DetectStreamerRequest {
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct DetectStreamerResponse {
    pub name: String,
}

pub async fn detect_streamer(
    State(service_register): State<ServiceRegister>,
    Json(payload): Json<DetectStreamerRequest>,
) -> Result<Json<DetectStreamerResponse>, Response> {
    let url = payload.url.trim().to_string();
    if url.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "请填写直播间链接").into_response());
    }

    let plugin = builtin_plugins()
        .into_iter()
        .find(|plugin| plugin.matches(&url))
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "不支持这个直播间链接").into_response())?;

    let worker = service_register.worker(
        LiveStreamer {
            id: -1,
            url,
            enabled: false,
            remark: String::new(),
            filename_prefix: None,
            time_range: None,
            upload_streamers_id: None,
            format: None,
            override_cfg: None,
            preprocessor: None,
            segment_processor: None,
            downloaded_processor: None,
            postprocessor: None,
            opt_args: None,
            excluded_keywords: None,
        },
        None,
    );

    match plugin.check_stream(live_request(&worker)).await {
        Ok(LiveStatus::Live { stream }) => {
            let name = stream.name.trim().to_string();
            if name.is_empty() {
                return Err((StatusCode::UNPROCESSABLE_ENTITY, "未识别到主播名称，请手动填写").into_response());
            }
            Ok(Json(DetectStreamerResponse { name }))
        }
        Ok(LiveStatus::Offline) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "直播间当前未开播，无法自动识别主播名称，请手动填写",
        )
            .into_response()),
        Err(error) => Err((
            StatusCode::BAD_GATEWAY,
            format!("主播名称识别失败：{error}"),
        )
            .into_response()),
    }
}
