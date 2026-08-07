#![cfg(mobile)]

use serde::{Deserialize, Serialize};
use tauri::{
    plugin::{Builder, PluginHandle, TauriPlugin},
    Manager, Runtime,
};

const PLUGIN_IDENTIFIER: &str = "app.tauri.livereplayandroid";

#[derive(Debug, Default, Serialize)]
struct EmptyPayload {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeAuthResult {
    pub authorized: bool,
    pub access_token: Option<String>,
    pub account_label: Option<String>,
    pub expires_at_millis: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeMp4Request {
    pub input_path: String,
    pub output_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeMp4Result {
    pub output_path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackgroundActiveRequest {
    active: bool,
}

pub struct LiveReplayAndroid<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> LiveReplayAndroid<R> {
    pub fn authorize_youtube(&self) -> Result<YoutubeAuthResult, String> {
        self.0
            .run_mobile_plugin("authorizeYoutube", EmptyPayload::default())
            .map_err(|error| error.to_string())
    }

    pub fn cached_youtube_auth(&self) -> Result<YoutubeAuthResult, String> {
        self.0
            .run_mobile_plugin("cachedYoutubeAuth", EmptyPayload::default())
            .map_err(|error| error.to_string())
    }

    pub fn logout_youtube(&self) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<()>("logoutYoutube", EmptyPayload::default())
            .map_err(|error| error.to_string())
    }

    pub fn set_background_active(&self, active: bool) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<()>("setBackgroundActive", BackgroundActiveRequest { active })
            .map_err(|error| error.to_string())
    }

    pub fn finalize_mp4(&self, request: FinalizeMp4Request) -> Result<FinalizeMp4Result, String> {
        self.0
            .run_mobile_plugin("finalizeMp4", request)
            .map_err(|error| error.to_string())
    }
}

pub trait LiveReplayAndroidExt<R: Runtime> {
    fn live_replay_android(&self) -> &LiveReplayAndroid<R>;
}

impl<R: Runtime, T: Manager<R>> LiveReplayAndroidExt<R> for T {
    fn live_replay_android(&self) -> &LiveReplayAndroid<R> {
        self.state::<LiveReplayAndroid<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("live-replay-android")
        .setup(|app, api| {
            let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "LiveReplayAndroidPlugin")?;
            app.manage(LiveReplayAndroid(handle));
            Ok(())
        })
        .build()
}
