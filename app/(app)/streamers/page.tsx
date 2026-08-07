'use client'

import React, { useEffect, useMemo, useState } from 'react'
import useSWR from 'swr'
import {
  Button,
  Card,
  Col,
  Form,
  Layout,
  List,
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
import { API_BASE, fetcher, ReplayStreamerState, ReplayUserState } from '../../lib/api-streamer'
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

const segmentMinutes = (value?: string) => {
  if (!value) return 60
  const match = /^(\d+):(\d{2}):(\d{2})$/.exec(value)
  if (!match) return 60
  return Number(match[1]) * 60 + Number(match[2]) + (Number(match[3]) >= 30 ? 1 : 0)
}

const segmentTime = (minutes: number) => {
  const safe = Math.max(1, Math.min(1440, Math.floor(minutes)))
  const h = Math.floor(safe / 60)
  const m = safe % 60
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:00`
}

const parseObject = (value?: string) => {
  try {
    return value ? JSON.parse(value) : {}
  } catch {
    return {}
  }
}

export default function StreamerSettingsPage() {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const { biliUsers } = useBiliUsers()
  const { data: legacy, isLoading: legacyLoading, mutate: refreshLegacy } = useSWR<any[]>('/v1/streamers', fetcher, { refreshInterval: 5000 })
  const { data: states, isLoading: stateLoading, mutate: refreshStates } = useSWR<ReplayStreamerState[]>('/v1/replay/streamers', fetcher, { refreshInterval: 3000 })
  const [editing, setEditing] = useState<{ streamer: any; template: any } | null>(null)
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

  const stateById = useMemo(
    () => new Map((states ?? []).map(item => [item.id, item])),
    [states],
  )

  const refreshAll = () => Promise.all([refreshLegacy(), refreshStates()])

  useEffect(() => {
    if (!editing || !formApi) return
    const extra = parseObject(editing.template.extra_fields)
    const override = editing.streamer.override ?? {}
    const userCookie = editing.template.user_cookie ?? accountOptions[0]?.value
    setEditAccount(userCookie)
    formApi.setValues({
      remark: editing.streamer.remark,
      user_cookie: userCookie,
      title: editing.template.title ?? '{streamer} 直播回放 %Y-%m-%d %H-%M',
      tid: editing.template.tid,
      tags_text: (editing.template.tags ?? []).join(','),
      copyright: editing.template.copyright ?? 2,
      copyright_source: editing.template.copyright_source ?? editing.streamer.url,
      description: editing.template.description ?? '',
      is_only_self: editing.template.is_only_self ?? 1,
      segment_minutes: segmentMinutes(override.segment_time),
      delete_after_success: extra.live_replay_delete_after_success !== false,
    })
  }, [editing, formApi])

  const openEdit = async (streamer: any) => {
    const state = stateById.get(streamer.id)
    if (state?.user_state === 'recording') {
      Notification.warning({ title: '正在录制', content: '为避免中途重启录制 Worker，请在本场直播结束后修改主播设置。' })
      return
    }
    if (!streamer.upload_streamers_id) {
      Notification.error({ title: '无法编辑', content: '这个旧主播没有关联投稿配置，建议删除后重新添加。' })
      return
    }
    try {
      const template = await fetcher(`/v1/upload/streamers/${streamer.upload_streamers_id}`)
      setEditing({ streamer, template })
    } catch (error: any) {
      Notification.error({ title: '读取设置失败', content: error?.message ?? String(error) })
    }
  }

  const saveEdit = async () => {
    if (!editing) return
    const values = await formApi?.validate()
    if (!values) return
    setSaving(true)
    try {
      const tags = String(values.tags_text ?? '')
        .split(/[，,]/)
        .map((item: string) => item.trim())
        .filter(Boolean)
      const previousExtra = parseObject(editing.template.extra_fields)
      const templatePayload = {
        ...editing.template,
        user_cookie: String(values.user_cookie ?? ''),
        title: String(values.title ?? '').trim(),
        tid: Number(values.tid),
        tags,
        copyright: Number(values.copyright),
        copyright_source: Number(values.copyright) === 2 ? String(values.copyright_source ?? '').trim() : null,
        description: String(values.description ?? '').trim(),
        is_only_self: Number(values.is_only_self),
        extra_fields: JSON.stringify({
          ...previousExtra,
          live_replay_delete_after_success: values.delete_after_success !== false,
        }),
      }

      const templateResponse = await fetch(`${API_BASE}/v1/upload/streamers`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(templatePayload),
      })
      if (!templateResponse.ok) throw new Error(await templateResponse.text())

      const streamerPayload = {
        ...editing.streamer,
        remark: String(values.remark ?? '').trim(),
        override: {
          ...(editing.streamer.override ?? {}),
          segment_time: segmentTime(Number(values.segment_minutes)),
        },
      }
      delete streamerPayload.status
      delete streamerPayload.upload_status
      delete streamerPayload.recording_elapsed_seconds
      delete streamerPayload.recording_bytes

      const streamerResponse = await fetch(`${API_BASE}/v1/streamers`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(streamerPayload),
      })
      if (!streamerResponse.ok) throw new Error(await streamerResponse.text())

      Notification.success({ title: '已保存', content: '新设置会用于下一次录制；当前待上传场次继续使用创建时冻结的投稿配置。' })
      setEditing(null)
      await refreshAll()
    } catch (error: any) {
      Notification.error({ title: '保存失败', content: error?.message ?? String(error), duration: 0 })
    } finally {
      setSaving(false)
    }
  }

  const toggle = async (id: number) => {
    setSwitching(previous => new Set(previous).add(id))
    try {
      const response = await fetch(`${API_BASE}/v1/streamers/${id}/pause`, { method: 'PUT' })
      if (!response.ok) throw new Error(await response.text())
      await refreshAll()
    } catch (error: any) {
      Notification.error({ title: '切换失败', content: error?.message ?? String(error) })
    } finally {
      setSwitching(previous => {
        const next = new Set(previous)
        next.delete(id)
        return next
      })
    }
  }

  const remove = async (id: number) => {
    try {
      const response = await fetch(`${API_BASE}/v1/streamers/${id}`, { method: 'DELETE' })
      if (!response.ok) throw new Error(await response.text())
      Notification.success({ title: '已删除主播' })
      await refreshAll()
    } catch (error: any) {
      Notification.error({
        title: '暂时不能删除',
        content: error?.message ?? '仍有未完成的录制或上传任务，请处理完成后再删除。',
        duration: 0,
      })
    }
  }

  if (legacyLoading || stateLoading) return <Spin size="large" />

  return (
    <>
      <Header style={{ backgroundColor: 'var(--semi-color-bg-1)' }}>
        <div style={{ minHeight: 64, padding: '0 24px', display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div style={{ display: 'flex', gap: 10, alignItems: 'center' }}>
            <IconVideoListStroked size="large" />
            <div><Title heading={4}>主播设置</Title><Text type="tertiary">只保留录制与投稿真正需要的选项</Text></div>
          </div>
          <Button icon={<IconRefresh />} onClick={refreshAll}>刷新</Button>
        </div>
      </Header>

      <Content style={{ padding: 24, backgroundColor: 'var(--semi-color-bg-0)' }}>
        <Row gutter={[16, 16]}>
          {(legacy ?? []).map(streamer => {
            const state = stateById.get(streamer.id)
            const userState = state?.user_state ?? 'waiting'
            return (
              <Col key={streamer.id} xs={24} sm={24} md={12} lg={8} xl={6}>
                <Card shadows="hover" title={streamer.remark} headerExtraContent={
                  <Switch
                    checked={streamer.enabled !== false}
                    disabled={switching.has(streamer.id)}
                    onChange={() => toggle(streamer.id)}
                  />
                }>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
                    <div><Tag color={stateColor[userState]}>{stateText[userState]}</Tag></div>
                    <Text ellipsis={{ showTooltip: true }} type="tertiary">{streamer.url}</Text>
                    {userState === 'recording' && (
                      <Text>已录制 {formatDuration(streamer.recording_elapsed_seconds)} · {formatBytes(streamer.recording_bytes)}</Text>
                    )}
                    {(state?.pending_upload_parts ?? 0) > 0 && <Text>待处理分段：{state?.pending_upload_parts}</Text>}
                    <div style={{ display: 'flex', gap: 8, marginTop: 6 }}>
                      <Button icon={<IconEdit2Stroked />} size="small" onClick={() => openEdit(streamer)}>编辑</Button>
                      <Popconfirm title="删除这个主播？" content="有未完成场次时系统会拒绝删除。" onConfirm={() => remove(streamer.id)}>
                        <Button icon={<IconDeleteStroked />} size="small" type="danger" theme="borderless">删除</Button>
                      </Popconfirm>
                    </div>
                  </div>
                </Card>
              </Col>
            )
          })}
        </Row>
      </Content>

      <Modal
        title={editing ? `编辑 ${editing.streamer.remark}` : '编辑主播'}
        visible={!!editing}
        confirmLoading={saving}
        onOk={saveEdit}
        onCancel={() => setEditing(null)}
        okText="保存"
        style={{ width: 680 }}
      >
        <Form getFormApi={setFormApi}>
          <Form.Input field="remark" label="主播名称" rules={[{ required: true, message: '请填写主播名称' }]} />
          <Form.Input field="source_url" label="直播间链接" initValue={editing?.streamer.url} disabled />
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
