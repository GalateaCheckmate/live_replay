const COMMANDS: &[&str] = &["authorize_youtube", "cached_youtube_auth", "logout_youtube", "finalize_mp4"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .try_build()
        .unwrap();
}
