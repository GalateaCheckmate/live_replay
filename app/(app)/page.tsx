'use client'

import { useEffect, useMemo, useState } from 'react'
import Link from 'next/link'
import useSWR from 'swr'
import { Button, Card, Col, Form, Layout, Modal, Notification, Row, Switch, Tag, Typography } from '@douyinfe/semi-ui'
import { IconPlusCircle, IconRefresh } from '@douyinfe/semi-icons'
import { useBiliUsers, useTypeTree } from '../lib/use-streamers'
import { API_BASE, fetcher, ReplayStreamerState, ReplayUserState } from '../lib/api-streamer'

const DEFAULT_TITLE = '{streamer} 直播回放 %Y-%m-%d %H-%M'

const stateText: Record<ReplayUserState, string> = {
  waiting: '等待开播',
  recording: '正在录制',
  uploading: '正在上传',
  error: '异常',
}

const stateColor: Record<ReplayUserState, 'green' | 'red' | 'blue' | 'orange'> = {
  waiting: 'green',
  recording: 'red',
  uploading: 'blue',
  error: 'orange',
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

interface ReplayGlobalSettings {
  segment_time?: string | null
}

const segmentMinutesFromConfig = (segmentTime?: string | null) => {
  const match = String(segmentTime ?? '').trim().match(/^(\d+):([0-5]\d):([0-5]\d)$/)
  if (!match) return 60
  const seconds = Number(match[1]) * 3600 + Number(match[2]) * 60 + Number(match[3])
  return Math.min(1440, Math.max(1, Math.ceil(seconds / 60)))
}

const formatDuration = (seconds?: number) => {
  const value = Math.max(0, Math.floor(seconds ?? 0))
  const h = Math.floor(value / 3600)
  const m = Math.floor((value % 3600) / 60)
  const s = value % 60
  return [h, m, s].map(item => String(item).padStart(2, '0')).join(':')
}

const formatBytes = (bytes?: number) => {
  const value = Math.max(0, bytes ?? 0)
  if (value < 1024) return `${value} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let current = value / 1024
  let index = 0
  while (current >= 1024 && index < units.length - 1) {
    current /= 1024
    index += 1
  }
  return `${current.toFixed(index >= 2 ? 2 : 1)} ${units[index]}`
}

export default function Home() {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const { biliUsers } = useBiliUsers()
  const {
    data: streamers,
    isLoading,
    mutate: refreshStreamers,
  } = useSWR<ReplayStreamerState[]>('/v1/replay/streamers', fetcher, { refreshInterval: 3000 })
  const { data: diskStatus, mutate: refreshDisk } = useSWR<DiskStatus>('/v1/replay/storage', fetcher, { refreshInterval: 10000 })
  const { data: globalSettings } = useSWR<ReplayGlobalSettings>('/v1/replay/settings', fetcher)
  const [visible, setVisible] = useState(false)
  const [saving, setSaving] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [formApi, setFormApi] = useState<any>()
  const [switching, setSwitching] = useState<Set<number>>(new Set())
  const [selectedAccount, setSelectedAccount] = useState<string>()
  const { typeTree, isLoading: typeTreeLoading, isError: typeTreeError } = useTypeTree(selectedAccount)

  const accountOptions = (biliUsers ?? []).map(item => ({ label: item.name, value: item.value }))
  const defaultSegmentMinutes = useMemo(
    () => segmentMinutesFromConfig(globalSettings?.segment_time),
    [globalSettings?.segment_time],
  )

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
    if (account) setSelectedAccount(account)
    queueMicrotask(() => {
      if (account) formApi?.setValue('user_cookie', account)
      formApi?.setValue('segment_minutes', defaultSegmentMinutes)
    })
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
      const response = await fetch(`${API_BASE}/v1/replay/streamers`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      })
      if (!response.ok) throw new Error(await response.text())
      Notification.success({ title: '主播已添加', content: '检测到开播后将自动开始录制。' })
      setVisible(false)
      formApi?.reset()
      await refreshStreamers()
    } catch (error: any) {
      Notification.error({ title: '添加失败', content: error?.message ?? String(error), duration: 0 })
    } finally {
      setSaving(false)
    }
  }

  const setStreamerEnabled = async (streamer: ReplayStreamerState) => {
    setSwitching(previous => new Set(previous).add(streamer.id))
    try {
      const response = await fetch(`${API_BASE}/v1/replay/streamers/${streamer.id}/enabled`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled: !streamer.enabled }),
      })
      if (!response.ok) throw new Error(await response.text())
      await refreshStreamers()
    } catch (error: any) {
      Notification.error({ title: '操作失败', content: error?.message ?? String(error) })
    } finally {
      setSwitching(previous => {
        const next = new Set(previous)
        next.delete(streamer.id)
        return next
      })
    }
  }

  const refreshAll = async () => {
    setRefreshing(true)
    try {
      const response = await fetch(`${API_BASE}/v1/replay/refresh`, { method: 'POST' })
      if (!response.ok) throw new Error(await response.text())
      const result = await response.json().catch(() => ({ checked: 0 }))
      await Promise.all([refreshStreamers(), refreshDisk()])
      Notification.success({
        title: '刷新完成',
        content: `已重新检查 ${Number(result?.checked ?? 0)} 个等待中的主播。`,
      })
    } catch (error: any) {
      Notification.error({ title: '刷新失败', content: error?.message ?? String(error) })
    } finally {
      setRefreshing(false)
    }
  }

  const diskAttention = diskStatus && diskStatus.state !== 'ok'

  return (
    <>
      <Header style={{ backgroundColor: 'var(--semi-color-bg-1)', padding: '0 24px' }}>
        <div style={{ minHeight: 64, display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, flexWrap: 'wrap' }}>
          <div>
            <Title heading={4}>主播</Title>
            <Text type="tertiary">自动监控开播状态，并完成录制与投稿</Text>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <Button icon={<IconRefresh />} loading={refreshing} onClick={refreshAll}>刷新</Button>
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
            <Title heading={4}>还没有主播</Title>
            <Text type="tertiary">添加主播后，Live Replay 会自动监控开播状态。</Text>
            <div style={{ marginTop: 20 }}><Button theme="solid" onClick={openAdd}>添加主播</Button></div>
          </Card>
        )}

        <Row gutter={[16, 16]}>
          {(streamers ?? []).map(streamer => (
            <Col key={streamer.id} xs={24} sm={24} md={12} lg={8} xl={6}>
              <Card
                shadows="hover"
                title={streamer.name}
                headerExtraContent={
                  <Switch
                    checked={streamer.enabled}
                    loading={switching.has(streamer.id)}
                    onChange={() => setStreamerEnabled(streamer)}
                  />
                }
              >
                <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
                  <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
                    <Tag color={stateColor[streamer.user_state]}>{stateText[streamer.user_state]}</Tag>
                    {!streamer.enabled && <Text type="tertiary">自动录制已关闭</Text>}
                  </div>
                  <Text ellipsis={{ showTooltip: true }} type="tertiary">{streamer.url}</Text>
                  {streamer.user_state === 'recording' && (
                    <Text>已录制 {formatDuration(streamer.recording_elapsed_seconds)} · {formatBytes(streamer.recording_bytes)}</Text>
                  )}
                  {streamer.pending_upload_parts > 0 && (
                    <Text>还有 {streamer.pending_upload_parts} 个录像分段正在处理</Text>
                  )}
                  {streamer.user_state === 'error' && (
                    <Text type="danger">有任务需要处理，请在“场次与投稿”中查看详情。</Text>
                  )}
                  <div style={{ display: 'flex', gap: 8, marginTop: 6 }}>
                    <Link href="/replay"><Button size="small">场次与投稿</Button></Link>
                    <Link href="/streamers"><Button size="small" theme="borderless">主播设置</Button></Link>
                  </div>
                </div>
              </Card>
            </Col>
          ))}
        </Row>
      </Content>

      <Modal
        title="添加主播"
        visible={visible}
        confirmLoading={saving}
        onOk={createStreamer}
        onCancel={() => setVisible(false)}
        okText="添加主播"
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
            segment_minutes: defaultSegmentMinutes,
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
          <Form.Input field="remark" label="主播名称" placeholder="填写便于识别的主播名称" rules={[{ required: true, message: '请填写主播名称' }]} />
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
            <Text type="tertiary">每场直播作为一个投稿；录像分段会按顺序追加为分P。</Text>
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
            <Text type="warning">未找到默认的“三角洲行动”分区，请手动选择。</Text>
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
          <Form.Input field="copyright_source" label="转载来源" placeholder="默认使用当前直播间链接；自制投稿时无需填写" />
          <Form.TextArea field="description" label="简介" placeholder="可选" autosize={{ minRows: 2, maxRows: 5 }} />
          <Form.InputNumber
            field="segment_minutes"
            label="单段时长（分钟）"
            min={1}
            max={1440}
            extraText={`默认使用全局设置：${defaultSegmentMinutes} 分钟。`}
            style={{ width: '100%' }}
          />
          <Form.Switch field="delete_after_success" label="投稿确认可播放后自动删除本地录像" />
        </Form>
      </Modal>
    </>
  )
}
