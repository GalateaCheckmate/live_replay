use biliup::uploader::bilibili::{Studio, Vid};
use biliup::uploader::util::SubmitOption;
use clap::{Parser, Subcommand};

use crate::UploadLine;
use std::path::PathBuf;

/// 扩展路径中的 ~ 为用户主目录
pub fn expand_path(path: PathBuf) -> PathBuf {
    if let Some(path_str) = path.to_str() {
        let expanded = shellexpand::tilde(path_str);
        return PathBuf::from(expanded.as_ref());
    }
    path
}

#[derive(Parser)]
#[command(author, version, about)]
pub struct Cli {
    #[clap(subcommand)]
    pub command: Commands,

    /// 配置代理
    #[arg(short, long, default_value = None)]
    pub proxy: Option<String>,

    /// 登录信息文件
    #[arg(short, long, default_value = "cookies.json")]
    pub user_cookie: PathBuf,

    #[arg(long, default_value = "tower_http=debug,info")]
    pub rust_log: String,
}

#[derive(Subcommand)]
pub enum Commands {
    /// 登录B站并保存登录信息
    Login,
    /// 手动验证并刷新登录信息
    Renew,
    /// 上传视频
    Upload {
        /// 提交接口
        #[arg(long)]
        submit: Option<SubmitOption>,

        /// 需要上传的视频路径；使用配置文件投稿时可省略
        #[arg()]
        video_path: Vec<PathBuf>,

        /// 投稿配置文件
        #[arg(short, long, value_name = "FILE")]
        config: Option<PathBuf>,

        /// 上传线路
        #[arg(short, long, value_enum)]
        line: Option<UploadLine>,

        /// 单文件上传并发数
        #[arg(long, default_value = "3")]
        limit: usize,

        #[command(flatten)]
        studio: Studio,
    },
    /// 向已有稿件追加视频
    Append {
        /// 提交接口
        #[arg(long)]
        submit: Option<SubmitOption>,

        /// 稿件 AV 号或 BV 号
        #[arg(short, long)]
        vid: Vid,

        /// 需要上传的视频路径
        #[arg()]
        video_path: Vec<PathBuf>,

        /// 上传线路
        #[arg(short, long, value_enum)]
        line: Option<UploadLine>,

        /// 单文件上传并发数
        #[arg(long, default_value = "3")]
        limit: usize,

        #[command(flatten)]
        studio: Studio,
    },
    /// 查看视频详情
    Show {
        /// 稿件 AV 号或 BV 号
        vid: Vid,
    },
    /// 查看视频评论
    Comments {
        /// 稿件 AV 号或 BV 号
        vid: Vid,

        /// 排序方式：0 按时间，2 按热度
        #[arg(long, default_value = "0")]
        sort: u8,

        /// 页码
        #[arg(long, default_value = "1")]
        pn: u32,

        /// 每页条数
        #[arg(long, default_value = "20")]
        ps: u32,
    },
    /// 回复视频评论；默认仅预览，不实际发送
    Reply {
        /// 稿件 AV 号或 BV 号
        vid: Vid,

        /// 评论 rpid
        rpid: u64,

        /// 回复内容
        message: String,

        /// 实际发送回复
        #[arg(long)]
        execute: bool,
    },
    /// 输出 FLV 元数据
    DumpFlv {
        #[arg()]
        file_name: PathBuf,
    },
    /// 下载视频
    Download {
        url: String,

        /// 输出文件名模板，例如 ./video/%Y-%m-%dT%H_%M_%S{title}
        #[arg(short, long, default_value = "{title}")]
        output: String,

        /// 按文件大小分割
        #[arg(long, value_parser = human_size)]
        split_size: Option<u64>,

        /// 按时长分割
        #[arg(long)]
        split_time: Option<humantime::Duration>,
    },
    /// 启动 Live Replay 服务，默认仅允许本机访问
    Server {
        /// 监听地址
        #[arg(short, long, default_value = "127.0.0.1")]
        bind: String,

        /// 服务端口
        #[arg(short, long, default_value = "19159")]
        port: u16,

        /// 开启登录密码认证
        #[arg(long, default_value = "false")]
        auth: bool,

        /// 使用配置文件启动录制
        #[arg(short, long, value_name = "FILE")]
        config: Option<PathBuf>,
    },
    /// 列出已上传的视频
    List {
        /// 仅显示处理中视频
        #[arg(long)]
        is_pubing: bool,

        /// 仅显示已通过视频
        #[arg(long)]
        pubed: bool,

        /// 仅显示未通过视频
        #[arg(long)]
        not_pubed: bool,

        /// 起始页码
        #[arg(short, long, default_value = "1")]
        from_page: u32,

        /// 最大页数
        #[arg(short, long)]
        max_pages: Option<u32>,
    },
}

fn human_size(s: &str) -> Result<u64, String> {
    let ret = match s.as_bytes() {
        [init @ .., b'K'] => parse_u8(init)? * 1000.0,
        [init @ .., b'M'] => parse_u8(init)? * 1000.0 * 1000.0,
        [init @ .., b'G'] => parse_u8(init)? * 1000.0 * 1000.0 * 1000.0,
        init => parse_u8(init)?,
    };
    Ok(ret as u64)
}

fn parse_u8(string: &[u8]) -> Result<f64, String> {
    let string = String::from_utf8_lossy(string);
    string
        .parse()
        .map_err(|e| format!("{string} is not ascii digit. {:?}", e))
}
