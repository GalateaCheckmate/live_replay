from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def replace_once(path: Path, old: str, new: str) -> None:
    text = path.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"expected block not found in {path}")
    path.write_text(text.replace(old, new, 1), encoding="utf-8")


replay = ROOT / "crates/biliup-cli/src/server/common/replay.rs"
replace_once(
    replay,
    '''fn ffprobe_program() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(directory) = exe.parent()
    {
        let bundled = directory.join(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        if bundled.exists() {
            return bundled;
        }
    }
    PathBuf::from(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    })
}
''',
    '''fn bundled_or_path_program(name: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(directory) = exe.parent()
    {
        let filename = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        let bundled = directory.join(filename);
        if bundled.exists() {
            return bundled;
        }
    }
    PathBuf::from(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    })
}

fn ffmpeg_program() -> PathBuf {
    bundled_or_path_program("ffmpeg")
}

fn ffprobe_program() -> PathBuf {
    bundled_or_path_program("ffprobe")
}
''',
)
replace_once(
    replay,
    '''    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "warning", "-y", "-i"]);
''',
    '''    let ffmpeg = ffmpeg_program();
    let mut command = Command::new(&ffmpeg);
    command.args(["-hide_banner", "-loglevel", "warning", "-y", "-i"]);
''',
)
replace_once(
    replay,
    '''    let status = command
        .status()
        .await
        .change_context(AppError::Custom("failed to start ffmpeg remux".to_string()))?;
''',
    '''    let status = command.status().await.change_context(AppError::Custom(format!(
        "无法启动 FFmpeg（{}）；源录像已保留并等待重试",
        ffmpeg.display()
    )))?;
''',
)

live_replay = ROOT / "LIVE_REPLAY.md"
replace_once(
    live_replay,
    '''录像仍采用直播源码流，不转码。上传前若文件不是 MP4，会调用 FFmpeg 使用：
''',
    '''录像仍采用直播源码流，不转码。Windows Full 便携包会在 `live-replay.exe` 同目录自带 `ffmpeg.exe` 与 `ffprobe.exe`；运行时优先使用这两个内置程序，不要求用户安装 FFmpeg 或配置 PATH。上传前若文件不是 MP4，会调用 FFmpeg 使用：
''',
)

notice = ROOT / "FFMPEG-NOTICE.txt"
notice.write_text(
    "FFmpeg notice for Live Replay\n\n"
    "The Windows portable package includes ffmpeg.exe and ffprobe.exe from BtbN/FFmpeg-Builds.\n"
    "Live Replay uses the win64 LGPL build and invokes FFmpeg as separate executables for container remux/probing.\n\n"
    "FFmpeg project: https://ffmpeg.org/\n"
    "FFmpeg source: https://git.ffmpeg.org/ffmpeg.git\n"
    "Windows builds: https://github.com/BtbN/FFmpeg-Builds\n"
    "FFmpeg licensing information: https://ffmpeg.org/legal.html\n"
    "GNU LGPL 2.1: https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html\n\n"
    "FFmpeg and its libraries are separate third-party software and remain under their respective licenses.\n",
    encoding="utf-8",
)

trigger = ROOT / ".github/full-trigger"
trigger.write_text(
    "Validate bundled portable ffmpeg/ffprobe resolution and package assembly.\n"
    "Triggered: 2026-08-07T16:12:00+08:00\n",
    encoding="utf-8",
)

print("bundled FFmpeg source patch applied")
