'use client'

import React, { useEffect, useMemo, useState } from 'react'
import useSWR from 'swr'
import {
  Button,
  Card,
  Col,
  Form,
  Layout,
  Modal,
  Notification,
  Popconfirm,
  Row,
  Spin,
  Switch,
  Tag,
  Typography,
} from '@douyinfe/semi-ui'
import { IconDeleteStroked, IconEdit2Stroked, IconRefresh, IconVideoListStroked } from '@douyinfe/semi-icons'
import {
  API_BASE,
  fetcher,
  ReplayStreamerSettings,
  ReplayStreamerState,
  ReplayUserState,
} from '../../lib/api-streamer'
import { useBiliUsers, useTypeTree } from '../../lib/use-streamers'

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

export default function StreamerSettingsPage() {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const { biliUsers } = useBiliUsers()
  const {
    data: streamers,
    isLoading,
    mutate: refreshStreamers,
  } = useSWR<ReplayStreamerState[]>('/v1/replay/streamers', fetcher, { refreshInterval: 3000 })
  const [editing, setEditing] = useState<ReplayStreamerSettings | null>(null)
  const [editAccount, setEditAccount] = useState<string>()
  const [formApi, setFormApi] = useState<any>()
  const [saving, setSaving] = useState(false)
  const [switching, setSwitching] = useState<Set<number>>(new Set())
  const { typeTree, isLoading: typeTreeLoading } = useTypeTree(editAccount)

  const accountOptions = (biliUsers ?? []).map(item => ({ label: item.name, value: item.value }))
  const partitionOptions = useMemo(() => {
    return (typeTree ?? []).flatMap((group: any) => {
      const children = group.children ?? []
      if (children.length === 0) return [{ label: group.label, value: Number(group.value) }]
      return children.map((child: any) => ({ label: `${group.label} / ${child.label}`, value: Number(child.value) }))
    })
  }, [typeTree])

  useEffect(() => {
    if (!editing || !formApi) return
    formApi.setValues({
      name: editing.name,
      source_url: editing.url,
      user_cookie: editing.user_cookie,
      title: editing.title,
      tid: editing.tid || undefined,
      tags_text: editing.tags.join(','),
      copyright: editing.copyright,
      copyright_source: editing.copyright_source || editing.url,
      description: editing.description,
      is_only_self: editing.is_only_self,
      segment_minutes: editing.segment_minutes,
      delete_after_success: editing.delete_after_success,
    })
  }, [editing, formApi])

  const openEdit = async (streamer: ReplayStreamerState) => {
    if (streamer.user_state === 'recording') {
      Notification.warning({
        title: '正在录制',
        content: '为避免中途重启录制 Worker，请在本场直播结束后修改主播设置。',
      })
      return
    }
    try {
      const settings = await fetcher(`/v1/replay/streamers/${streamer.id}/settings`) as ReplayStreamerSettings
      setEditAccount(settings.user_cookie || accountOptions[0]?.value)
      setEditing(settings)
    } catch (error: any) {
      Notification.error({ title: '读取设置失败', content: error?.message ?? String(error) })
    }
  }

  const saveEdit = async () => {
    if (!editing) return
    const values = await formApi?.validate()
    if (!values) return

    const tags = String(values.tags_text ?? '')
      .split(/[，,]/)
      .map((item: string) => item.trim())
      .filter(Boolean)

    const payload = {
      name: String(values.name ?? '').trim(),
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
      const response = await fetch(`${API_BASE}/v1/replay/streamers/${editing.id}/settings`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      })
      if (!response.ok) throw new Error(await response.text())
      Notification.success({
        title: '已保存',
        content: '新设置用于之后的新场次；当前待上传场次继续使用创建时冻结的投稿配置。',
      })
      setEditing(null)
      formApi?.reset()
      await refreshStreamers()
    } catch (error: any) {
      Notification.error({ title: '保存失败', content: error?.message ?? String(error), duration: 0 })
    } finally {
      setSaving(false)
    }
  }

  const setEnabled = async (streamer: ReplayStreamerState) => {
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
      Notification.error({ title: '切换失败', content: error?.message ?? String(error) })
    } finally {
      setSwitching(previous => {
        const next = new Set(previous)
        next.delete(streamer.id)
        return next
      })
    }
  }

  const remove = async (id: number) => {
    try {
      const response = await fetch(`${API_BASE}/v1/replay/streamers/${id}`, { method: 'DELETE' })
      if (!response.ok) throw new Error(await response.text())
      Notification.success({ title: '已删除主播' })
      await refreshStreamers()
    } catch (error: any) {
      Notification.error({
        title: '暂时不能删除',
        content: error?.message ?? '仍有未完成的录制或上传任务，请处理完成后再删除。',
        duration: 0,
      })
    }
  }

  if (isLoading) return <Spin size="large" />

  return (
    <>
      <Header style={{ backgroundColor: 'var(--semi-color-bg-1)' }}>
        <div style={{ minHeight: 64, padding: '0 24px', display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div style={{ display: 'flex', gap: 10, alignItems: 'center' }}>
            <IconVideoListStroked size="large" />
            <div><Title heading={4}>主播设置</Title><Text type="tertiary">只保留录制与投稿真正需要的选项</Text></div>
          </div>
          <Button icon={<IconRefresh />} onClick={() => refreshStreamers()}>刷新</Button>
        </div>
      </Header>

      <Content style={{ padding: 24, backgroundColor: 'var(--semi-color-bg-0)' }}>
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
                    onChange={() => setEnabled(streamer)}
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
                  {streamer.pending_upload_parts > 0 && <Text>待处理分段：{streamer.pending_upload_parts}</Text>}
                  <div style={{ display: 'flex', gap: 8, marginTop: 6 }}>
                    <Button icon={<IconEdit2Stroked />} size="small" onClick={() => openEdit(streamer)}>编辑</Button>
                    <Popconfirm
                      title="删除这个主播？"
                      content="有未完成场次时系统会拒绝删除。"
                      onConfirm={() => remove(streamer.id)}
                    >
                      <Button icon={<IconDeleteStroked />} size="small" type="danger" theme="borderless">删除</Button>
                    </Popconfirm>
                  </div>
                </div>
              </Card>
            </Col>
          ))}
        </Row>
      </Content>

      <Modal
        title={editing ? `编辑 ${editing.name}` : '编辑主播'}
        visible={!!editing}
        confirmLoading={saving}
        onOk={saveEdit}
        onCancel={() => {
          setEditing(null)
          formApi?.reset()
        }}
        okText="保存"
        style={{ width: 680 }}
      >
        <Form getFormApi={setFormApi}>
          <Form.Input field="name" label="主播名称" rules={[{ required: true, message: '请填写主播名称' }]} />
          <Form.Input field="source_url" label="直播间链接" disabled />
          <Form.Select
            field="user_cookie"
            label="投稿账号"
            optionList={accountOptions}
            onChange={(value: any) => {
              setEditAccount(String(value))
              formApi?.setValue('tid', undefined)
            }}
            rules={[{ required: true, message: '请选择投稿账号' }]}
            style={{ width: '100%' }}
          />
          <Form.Input field="title" label="视频标题" rules={[{ required: true, message: '请填写标题格式' }]} />
          <Form.Select
            field="tid"
            label="视频分区"
            optionList={partitionOptions}
            loading={typeTreeLoading}
            rules={[{ required: true, message: '请选择视频分区' }]}
            style={{ width: '100%' }}
          />
          <Form.Input field="tags_text" label="视频标签" placeholder="游戏,直播回放" />
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
          <Form.Input field="copyright_source" label="转载来源" />
          <Form.TextArea field="description" label="简介" autosize={{ minRows: 2, maxRows: 5 }} />
          <Form.InputNumber field="segment_minutes" label="单段时长（分钟）" min={1} max={1440} style={{ width: '100%' }} />
          <Form.Switch field="delete_after_success" label="B站确认可播放后删除本地录像" />
          <Card style={{ marginTop: 12 }}>
            <Text type="tertiary">直播间地址暂不允许在编辑中修改，避免正在监控时切换来源造成场次混淆。</Text>
          </Card>
        </Form>
      </Modal>
    </>
  )
}
