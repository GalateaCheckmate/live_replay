from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def replace_once(path: str, old: str, new: str) -> None:
    file = ROOT / path
    text = file.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"pattern not found in {path}: {old[:120]!r}")
    file.write_text(text.replace(old, new, 1), encoding="utf-8")


# 1) B站分区：使用当前选中的已登录账号查询 archive/pre。
replace_once(
    "crates/biliup-cli/src/server/api/bilibili_endpoints.rs",
    """pub async fn archive_pre_endpoint(\n    Query(_params): Query<HashMap<String, String>>,\n    State(pool): State<ConnectionPool>,\n) -> Result<Json<serde_json::Value>, Response> {\n    // 获取所有B站Cookie配置\n""",
    """pub async fn archive_pre_endpoint(\n    Query(params): Query<HashMap<String, String>>,\n    State(pool): State<ConnectionPool>,\n) -> Result<Json<serde_json::Value>, Response> {\n    // 添加主播时优先使用用户当前选择的已登录账号获取分区。\n    // 这样不同账号看到的投稿能力/分区信息与真正投稿账号保持一致。\n    if let Some(user) = params.get(\"user\").map(String::as_str).map(str::trim).filter(|v| !v.is_empty()) {\n        let bili = login_by_cookies(user, None)\n            .await\n            .change_context(AppError::Custom(\"所选B站账号登录状态已失效\".to_string()))\n            .map_err(report_to_response)?;\n        return Ok(Json(\n            bili.archive_pre()\n                .await\n                .change_context(AppError::Unknown)\n                .map_err(report_to_response)?,\n        ));\n    }\n\n    // 未指定账号时兼容旧页面：获取所有B站Cookie配置\n""",
)

# 2) useTypeTree 支持指定账号，并把子分区统一转换成可用于 Select 的结构。
use_streamers = ROOT / "app/lib/use-streamers.ts"
text = use_streamers.read_text(encoding="utf-8")
old = """export function useTypeTree() {\n  const { data: archivePre, error, isLoading } = useSWR(\"/bili/archive/pre\", fetcher);\n  const treeData = archivePre?.data?.typelist.map((type: BiliType)=> {\n    return {\n      label: type.name,\n      value: type.id,\n      children: type.children\n    };\n  });\n  return {\n    isLoading,\n    isError: error,\n    typeTree: treeData,\n  };\n}\n"""
new = """export function useTypeTree(userCookie?: string) {\n  const key = userCookie\n    ? `/bili/archive/pre?user=${encodeURIComponent(userCookie)}`\n    : '/bili/archive/pre';\n  const { data: archivePre, error, isLoading } = useSWR(key, fetcher);\n\n  const mapType = (type: BiliType): any => ({\n    label: type.name,\n    value: type.id,\n    name: type.name,\n    id: type.id,\n    children: (type.children ?? []).map(mapType),\n  });\n  const types = archivePre?.data?.typelist;\n  const treeData = Array.isArray(types) ? types.map(mapType) : [];\n\n  return {\n    isLoading,\n    isError: error,\n    typeTree: treeData,\n  };\n}\n"""
if old not in text:
    raise SystemExit("useTypeTree pattern not found")
use_streamers.write_text(text.replace(old, new, 1), encoding="utf-8")

# 3) 首页添加主播：所有投稿参数都是可编辑的默认值，不再强制三角洲分区。
home = ROOT / "app/(app)/page.tsx"
home.write_text(r'''\'use client\'

import { useEffect, useMemo, useState } from 'react'
import Link from 'next/link'
import useSWR from 'swr'
import { Button, Card, Col, Form, Layout, Modal, Notification, Row, Switch, Tag, Typography } from '@douyinfe/semi-ui'
import { IconPlusCircle, IconRefresh } from '@douyinfe/semi-icons'
import { useSWRConfig } from 'swr'
import useStreamers, { useBiliUsers, useTypeTree } from '../lib/use-streamers'
import { API_BASE, fetcher } from '../lib/api-streamer'

const DEFAULT_TITLE = '{streamer} 直播回放 %Y-%m-%d %H-%M'

const statusText: Record<string, string> = {
  Working: '正在录制',
  Pending: '检测直播状态',
  Idle: '等待开播',
  Pause: '已关闭',
  Finalizing: '正在收尾并封段',
}

const statusColor: Record<string, 'red' | 'blue' | 'green' | 'grey' | 'orange'> = {
  Working: 'red',
  Pending: 'blue',
  Idle: 'green',
  Pause: 'grey',
  Finalizing: 'orange',
}

interface DiskStatus {
  directory: string
  free_bytes?: number
  free_gb?: number
  warning_gb: number
  stop_gb: number
  state: 'ok' | 'warning' | 'blocked' | 'unknown'
  message: string
}

export default function Home() {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const { streamers, isLoading } = useStreamers()
  const { biliUsers } = useBiliUsers()
  const { mutate } = useSWRConfig()
  const [visible, setVisible] = useState(false)
  const [saving, setSaving] = useState(false)
  const [formApi, setFormApi] = useState<any>()
  const [finalizing, setFinalizing] = useState<Set<number>>(new Set())
  const [selectedAccount, setSelectedAccount] = useState<string>()
  const { typeTree, isLoading: typeTreeLoading, isError: typeTreeError } = useTypeTree(selectedAccount)
  const { data: diskStatus, mutate: refreshDisk } = useSWR<DiskStatus>('/v1/disk-status', fetcher, { refreshInterval: 10000 })

  const accountOptions = (biliUsers ?? []).map(item => ({ label: item.name, value: item.value }))

  useEffect(() => {
    if (!selectedAccount && accountOptions[0]?.value) {
      setSelectedAccount(accountOptions[0].value)
      formApi?.setValue('user_cookie', accountOptions[0].value)
    }
  }, [accountOptions, formApi, selectedAccount])

  const partitionOptions = useMemo(() => {
    return (typeTree ?? []).flatMap((group: any) => {
      const children = group.children ?? []
      if (children.length === 0) {
        return [{ label: group.label, value: Number(group.value), name: group.name }]
      }
      return children.map((child: any) => ({
        label: `${group.label} / ${child.label}`,
        value: Number(child.value),
        name: child.name,
      }))
    })
  }, [typeTree])

  const deltaForceTid = useMemo(
    () => partitionOptions.find((item: any) => item.name?.trim() === '三角洲行动')?.value as number | undefined,
    [partitionOptions],
  )

  useEffect(() => {
    if (!formApi || deltaForceTid === undefined) return
    const currentTid = formApi.getValue?.('tid')
    if (!currentTid) formApi.setValue('tid', deltaForceTid)
  }, [deltaForceTid, formApi, selectedAccount])

  const openAdd = () => {
    setVisible(true)
    const account = selectedAccount ?? accountOptions[0]?.value
    if (account) {
      setSelectedAccount(account)
      queueMicrotask(() => formApi?.setValue('user_cookie', account))
    }
  }

  const createStreamer = async () => {
    const values = await formApi?.validate()
    if (!values) return

    const tags = String(values.tags_text ?? '')
      .split(/[，,]/)
      .map((item: string) => item.trim())
      .filter(Boolean)

    const body = {
      url: String(values.url ?? '').trim(),
      remark: String(values.remark ?? '').trim(),
      user_cookie: String(values.user_cookie ?? '').trim(),
      title: String(values.title ?? '').trim(),
      tid: Number(values.tid),
      tags,
      copyright: Number(values.copyright),
      copyright_source: String(values.copyright_source ?? '').trim(),
      description: String(values.description ?? '').trim(),
      is_only_self: Number(values.is_only_self),
      segment_minutes: Number(values.segment_minutes),
      delete_after_success: values.delete_after_success !== false,
    }

    setSaving(true)
    try {
      const response = await fetch(`${API_BASE}/v1/streamers/simple`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      })
      if (!response.ok) throw new Error(await response.text())
      Notification.success({ title: '添加成功', content: '已持续关注；开播后会自动录制并按当前设置投稿。' })
      setVisible(false)
      formApi?.reset()
      await mutate('/v1/streamers')
    } catch (error: any) {
      Notification.error({ title: '添加失败', content: error?.message ?? String(error), duration: 0 })
    } finally {
      setSaving(false)
    }
  }

  const toggleStreamer = async (id: number) => {
    const streamer = streamers?.find(item => item.id === id)
    const isDisabling = streamer?.enabled !== false
    if (isDisabling) setFinalizing(previous => new Set(previous).add(id))
    try {
      const response = await fetch(`${API_BASE}/v1/streamers/${id}/pause`, { method: 'PUT' })
      if (!response.ok) {
        Notification.error({ title: '切换失败', content: await response.text() })
        return
      }
      await mutate('/v1/streamers')
    } finally {
      setFinalizing(previous => {
        const next = new Set(previous)
        next.delete(id)
        return next
      })
    }
  }

  const diskAttention = diskStatus && diskStatus.state !== 'ok'

  return (
    <>
      <Header style={{ backgroundColor: 'var(--semi-color-bg-1)', padding: '0 24px' }}>
        <div style={{ height: 64, display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div>
            <Title heading={4}>Live Replay</Title>
            <Text type="tertiary">一个开关完成持续关注、自动录制和自动上传</Text>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <Button icon={<IconRefresh />} onClick={() => Promise.all([mutate('/v1/streamers'), refreshDisk()])}>刷新</Button>
            <Button theme="solid" icon={<IconPlusCircle />} onClick={openAdd}>添加主播</Button>
          </div>
        </div>
      </Header>
      <Content style={{ padding: 24, backgroundColor: 'var(--semi-color-bg-0)' }}>
        {diskAttention && (
          <Card style={{ marginBottom: 16, borderColor: diskStatus.state === 'warning' ? 'var(--semi-color-warning)' : 'var(--semi-color-danger)' }}>
            <Text type={diskStatus.state === 'warning' ? 'warning' : 'danger'} strong>{diskStatus.message}</Text><br />
            <Text type="tertiary">录像目录：{diskStatus.directory}</Text>
          </Card>
        )}
        {!isLoading && (streamers?.length ?? 0) === 0 && (
          <Card style={{ maxWidth: 720, margin: '48px auto', textAlign: 'center' }}>
            <Title heading={4}>还没有关注主播</Title>
            <Text type="tertiary">粘贴直播间链接后，软件会一直等待开播并自动完成后续流程。</Text>
            <div style={{ marginTop: 20 }}><Button theme="solid" onClick={openAdd}>添加第一个主播</Button></div>
          </Card>
        )}
        <Row gutter={[16, 16]}>
          {(streamers ?? []).map(streamer => {
            const isFinalizing = finalizing.has(streamer.id)
            const status = isFinalizing ? 'Finalizing' : (streamer.enabled === false ? 'Pause' : (streamer.status || 'Idle'))
            return (
              <Col key={streamer.id} xs={24} sm={24} md={12} lg={8} xl={6}>
                <Card shadows="hover" title={streamer.remark} headerExtraContent={
                  <Switch checked={streamer.enabled !== false} disabled={isFinalizing} onChange={() => toggleStreamer(streamer.id)} />
                }>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
                    <div><Tag color={statusColor[status] ?? 'grey'}>{statusText[status] ?? status}</Tag></div>
                    <Text ellipsis={{ showTooltip: true }} type="tertiary">{streamer.url}</Text>
                    <Text>自动行为：录制 → 上传 → B站可播放 → 按主播设置处理本地视频</Text>
                    <div style={{ display: 'flex', gap: 8, marginTop: 6 }}>
                      <Link href="/replay"><Button size="small">查看上传队列</Button></Link>
                      <Link href="/streamers"><Button size="small" theme="borderless">详细设置</Button></Link>
                    </div>
                  </div>
                </Card>
              </Col>
            )
          })}
        </Row>
      </Content>

      <Modal
        title="添加主播"
        visible={visible}
        confirmLoading={saving}
        onOk={createStreamer}
        onCancel={() => setVisible(false)}
        okText="开始持续关注"
        style={{ width: 680 }}
      >
        <Form
          getFormApi={setFormApi}
          initValues={{
            title: DEFAULT_TITLE,
            tags_text: '游戏',
            copyright: 2,
            is_only_self: 1,
            description: '',
            segment_minutes: 60,
            delete_after_success: true,
          }}
        >
          <Form.Input
            field="url"
            label="直播间链接"
            placeholder="粘贴抖音、B站或斗鱼直播间链接"
            onChange={(value: any) => formApi?.setValue('copyright_source', String(value ?? ''))}
            rules={[{ required: true, message: '请填写直播间链接' }]}
          />
          <Form.Input field="remark" label="主播名称" placeholder="例如：小天才" rules={[{ required: true, message: '请填写主播名称' }]} />
          <Form.Select
            field="user_cookie"
            label="投稿账号"
            optionList={accountOptions}
            onChange={(value: any) => {
              setSelectedAccount(String(value))
              formApi?.setValue('tid', undefined)
            }}
            rules={[{ required: true, message: '请先登录B站账号' }]}
            style={{ width: '100%' }}
          />

          <Card style={{ margin: '12px 0' }}>
            <Text strong>投稿设置</Text><br />
            <Text type="tertiary">下面都是默认预填值，不是强制规则；每个主播都可以单独修改。</Text>
          </Card>

          <Form.Input field="title" label="视频标题" rules={[{ required: true, message: '请填写视频标题格式' }]} />
          <Form.Select
            field="tid"
            label="视频分区"
            optionList={partitionOptions}
            loading={typeTreeLoading}
            placeholder={typeTreeError ? '分区获取失败，请重新选择账号或刷新' : '请选择B站分区'}
            rules={[{ required: true, message: '请选择视频分区' }]}
            style={{ width: '100%' }}
          />
          {deltaForceTid === undefined && !typeTreeLoading && (
            <Text type="warning">当前账号没有匹配到默认“三角洲行动”，请从上面的B站分区列表手动选择；不会阻止添加主播。</Text>
          )}
          <Form.Input field="tags_text" label="视频标签" placeholder="多个标签用逗号分隔，例如：游戏,直播回放" />
          <Form.Select
            field="is_only_self"
            label="可见范围"
            optionList={[{ label: '仅自己可见', value: 1 }, { label: '公开', value: 0 }]}
            style={{ width: '100%' }}
          />
          <Form.Select
            field="copyright"
            label="投稿类型"
            optionList={[{ label: '转载', value: 2 }, { label: '自制', value: 1 }]}
            style={{ width: '100%' }}
          />
          <Form.Input field="copyright_source" label="转载来源" placeholder="默认使用当前直播间链接；自制投稿时忽略" />
          <Form.TextArea field="description" label="简介" placeholder="默认留空" autosize={{ minRows: 2, maxRows: 5 }} />
          <Form.InputNumber field="segment_minutes" label="单段时长（分钟）" min={1} max={1440} style={{ width: '100%' }} />
          <Form.Switch field="delete_after_success" label="B站确认可播放后删除本地录像" />
        </Form>
      </Modal>
    </>
  )
}
''', encoding='utf-8')

# 修正上面 raw string 开头转义成真正的 'use client'。
text = home.read_text(encoding='utf-8')
if text.startswith("\\'use client\\'"):
    text = "'use client'" + text[len("\\'use client\\'"):]
home.write_text(text, encoding='utf-8')

# 4) 后端简单主播请求接收所有可编辑默认项，并写入该主播自己的上传配置。
replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    "use crate::server::config::Config;",
    "use crate::server::config::{Config, ConfigPatch};",
)

replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """#[derive(Deserialize)]\npub struct SimpleStreamerRequest {\n    pub url: String,\n    pub remark: String,\n    pub user_cookie: String,\n    pub tid: u16,\n}\n""",
    """fn default_simple_title() -> String {\n    \"{streamer} 直播回放 %Y-%m-%d %H-%M\".to_string()\n}\n\nfn default_simple_tags() -> Vec<String> {\n    vec![\"游戏\".to_string()]\n}\n\nfn default_simple_copyright() -> u8 {\n    2\n}\n\nfn default_simple_visibility() -> u8 {\n    1\n}\n\nfn default_simple_segment_minutes() -> u64 {\n    60\n}\n\nfn default_true() -> bool {\n    true\n}\n\n#[derive(Deserialize)]\npub struct SimpleStreamerRequest {\n    pub url: String,\n    pub remark: String,\n    pub user_cookie: String,\n    pub tid: u16,\n    #[serde(default = \"default_simple_title\")]\n    pub title: String,\n    #[serde(default = \"default_simple_tags\")]\n    pub tags: Vec<String>,\n    #[serde(default = \"default_simple_copyright\")]\n    pub copyright: u8,\n    #[serde(default)]\n    pub copyright_source: String,\n    #[serde(default)]\n    pub description: String,\n    #[serde(default = \"default_simple_visibility\")]\n    pub is_only_self: u8,\n    #[serde(default = \"default_simple_segment_minutes\")]\n    pub segment_minutes: u64,\n    #[serde(default = \"default_true\")]\n    pub delete_after_success: bool,\n}\n""",
)

old = """    let upload = ormlite::Insert::insert(\n        InsertUploadStreamer {\n            id: None,\n            template_name: format!(\"live-replay:{}:{}\", remark, Utc::now().timestamp_millis()),\n            title: Some(\"{streamer} 直播回放 %Y-%m-%d %H-%M\".to_string()),\n            tid: Some(payload.tid),\n            copyright: Some(2),\n            copyright_source: Some(url.clone()),\n            cover_path: None,\n            description: Some(String::new()),\n            dynamic: None,\n            dtime: None,\n            dolby: None,\n            hires: None,\n            charging_pay: None,\n            no_reprint: None,\n            uploader: Some(\"biliup-rs\".to_string()),\n            user_cookie: Some(user_cookie),\n            tags: vec![\"三角洲行动\".to_string(), \"游戏\".to_string()],\n            credits: None,\n            up_selection_reply: None,\n            up_close_reply: None,\n            up_close_danmu: None,\n            extra_fields: None,\n            is_only_self: Some(1),\n        },\n        &pool,\n    )\n"""
new = """    if payload.tid == 0 {\n        return Err((StatusCode::BAD_REQUEST, \"请选择有效的B站视频分区\").into_response());\n    }\n    if !matches!(payload.copyright, 1 | 2) {\n        return Err((StatusCode::BAD_REQUEST, \"投稿类型只能是自制或转载\").into_response());\n    }\n    if payload.is_only_self > 1 {\n        return Err((StatusCode::BAD_REQUEST, \"可见范围参数无效\").into_response());\n    }\n    if !(1..=1440).contains(&payload.segment_minutes) {\n        return Err((StatusCode::BAD_REQUEST, \"单段时长必须在1到1440分钟之间\").into_response());\n    }\n\n    let title = if payload.title.trim().is_empty() {\n        default_simple_title()\n    } else {\n        payload.title.trim().to_string()\n    };\n    let tags = {\n        let mut tags: Vec<String> = payload\n            .tags\n            .into_iter()\n            .map(|value| value.trim().to_string())\n            .filter(|value| !value.is_empty())\n            .collect();\n        tags.dedup();\n        if tags.is_empty() {\n            tags.push(\"游戏\".to_string());\n        }\n        tags\n    };\n    let copyright_source = if payload.copyright == 2 {\n        Some(if payload.copyright_source.trim().is_empty() {\n            url.clone()\n        } else {\n            payload.copyright_source.trim().to_string()\n        })\n    } else {\n        None\n    };\n    let hours = payload.segment_minutes / 60;\n    let minutes = payload.segment_minutes % 60;\n    let segment_time = format!(\"{hours:02}:{minutes:02}:00\");\n    let override_cfg: ConfigPatch = serde_json::from_value(json!({ \"segment_time\": segment_time }))\n        .change_context(AppError::Unknown)\n        .map_err(report_to_response)?;\n\n    let upload = ormlite::Insert::insert(\n        InsertUploadStreamer {\n            id: None,\n            template_name: format!(\"live-replay:{}:{}\", remark, Utc::now().timestamp_millis()),\n            title: Some(title),\n            tid: Some(payload.tid),\n            copyright: Some(payload.copyright),\n            copyright_source,\n            cover_path: None,\n            description: Some(payload.description.trim().to_string()),\n            dynamic: None,\n            dtime: None,\n            dolby: None,\n            hires: None,\n            charging_pay: None,\n            no_reprint: None,\n            uploader: Some(\"biliup-rs\".to_string()),\n            user_cookie: Some(user_cookie),\n            tags,\n            credits: None,\n            up_selection_reply: None,\n            up_close_reply: None,\n            up_close_danmu: None,\n            extra_fields: Some(\n                json!({ \"live_replay_delete_after_success\": payload.delete_after_success })\n                    .to_string(),\n            ),\n            is_only_self: Some(payload.is_only_self),\n        },\n        &pool,\n    )\n"""
replace_once("crates/biliup-cli/src/server/api/endpoints.rs", old, new)

replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """        override_cfg: None,\n        preprocessor: None,\n""",
    """        override_cfg: Some(override_cfg),\n        preprocessor: None,\n""",
)

# 5) 每个主播的“可播放后删除”设置覆盖全局默认。
replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """    let delete_after_success = env_bool(\"LIVE_REPLAY_DELETE_AFTER_SUCCESS\", true);\n    let preserve_danmaku = env_bool(\"LIVE_REPLAY_PRESERVE_DANMAKU\", false);\n""",
    """    let delete_after_success = upload_config\n        .extra_fields\n        .as_deref()\n        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())\n        .and_then(|value| {\n            value\n                .get(\"live_replay_delete_after_success\")\n                .and_then(serde_json::Value::as_bool)\n        })\n        .unwrap_or_else(|| env_bool(\"LIVE_REPLAY_DELETE_AFTER_SUCCESS\", true));\n    let preserve_danmaku = env_bool(\"LIVE_REPLAY_PRESERVE_DANMAKU\", false);\n""",
)

# 6) 手动上传不再 fire-and-forget：请求等待真实上传和投稿结果，错误原样回页面。
old = """    info!(\"通过页面开始上传\");\n    tokio::spawn(async move {\n        let (bilibili, videos) = upload(\n            upload_config\n                .user_cookie\n                .as_deref()\n                .unwrap_or(\"cookies.json\"),\n            None,\n            line,\n            &json_data.files,\n            limit as usize,\n        )\n        .await?;\n        if !videos.is_empty() {\n            let recorder = Recorder::new(\n                upload_config.title.clone(),\n                StreamerInfo::new(\n                    &upload_config.template_name,\n                    \"stream_title\",\n                    \"\",\n                    Utc::now(),\n                    \"\",\n                ),\n            );\n            let studio = build_studio(&upload_config, &bilibili, videos, &recorder).await?;\n            let response_data =\n                submit_to_bilibili(&bilibili, &studio, submit_api.as_deref()).await?;\n            info!(\"通过页面上传成功 {:?}\", response_data);\n        }\n        Ok::<_, Report<AppError>>(())\n    });\n\n    Ok(Json(serde_json::json!({})))\n"""
new = """    if json_data.files.is_empty() {\n        return Err((StatusCode::BAD_REQUEST, \"请至少选择一个视频文件\").into_response());\n    }\n\n    info!(files = ?json_data.files, \"通过页面开始上传\");\n    let (bilibili, videos) = upload(\n        upload_config\n            .user_cookie\n            .as_deref()\n            .unwrap_or(\"cookies.json\"),\n        None,\n        line,\n        &json_data.files,\n        limit as usize,\n    )\n    .await\n    .map_err(report_to_response)?;\n\n    if videos.is_empty() {\n        return Err((StatusCode::BAD_REQUEST, \"没有成功上传任何视频文件\").into_response());\n    }\n\n    let recorder = Recorder::new(\n        upload_config.title.clone(),\n        StreamerInfo::new(\n            &upload_config.template_name,\n            \"stream_title\",\n            \"\",\n            Utc::now(),\n            \"\",\n        ),\n    );\n    let studio = build_studio(&upload_config, &bilibili, videos, &recorder)\n        .await\n        .map_err(report_to_response)?;\n    let response_data = submit_to_bilibili(&bilibili, &studio, submit_api.as_deref())\n        .await\n        .map_err(report_to_response)?;\n    info!(\"通过页面上传成功 {:?}\", response_data);\n\n    Ok(Json(json!({ \"ok\": true })))\n"""
replace_once("crates/biliup-cli/src/server/api/endpoints.rs", old, new)

# 7) 手动上传页面：上传中保持窗口，成功才关闭，失败显示真实错误。
replace_once(
    "app/(app)/upload-manager/page.tsx",
    """  const [selectFiles, setSelectFiles] = useState<(string | number)[]>([])\n  const [selectEntity, setSelectEntity] = useState<StudioEntity>()\n""",
    """  const [selectFiles, setSelectFiles] = useState<(string | number)[]>([])\n  const [selectEntity, setSelectEntity] = useState<StudioEntity>()\n  const [uploading, setUploading] = useState(false)\n""",
)

replace_once(
    "app/(app)/upload-manager/page.tsx",
    """  const handleOk = async () => {\n    await sendRequest('/v1/uploads', {\n      arg: {\n        files: selectFiles,\n        params: selectEntity,\n      },\n    })\n    setVisibleModal(false)\n  }\n""",
    """  const handleOk = async () => {\n    if (selectFiles.length === 0 || !selectEntity) {\n      Notification.warning({ title: '请选择文件', content: '至少选择一个视频文件后再上传。' })\n      return\n    }\n    setUploading(true)\n    try {\n      await sendRequest('/v1/uploads', {\n        arg: {\n          files: selectFiles,\n          params: selectEntity,\n        },\n      })\n      Notification.success({ title: '上传成功', content: '视频文件和投稿信息均已成功提交到B站。' })\n      setVisibleModal(false)\n      setSelectFiles([])\n      setTransferData([])\n    } catch (error: any) {\n      Notification.error({\n        title: '上传失败',\n        content: error?.message ?? String(error),\n        duration: 0,\n      })\n    } finally {\n      setUploading(false)\n    }\n  }\n""",
)

replace_once(
    "app/(app)/upload-manager/page.tsx",
    """        visible={visibleModal}\n        onOk={handleOk}\n""",
    """        visible={visibleModal}\n        confirmLoading={uploading}\n        onOk={handleOk}\n""",
)
