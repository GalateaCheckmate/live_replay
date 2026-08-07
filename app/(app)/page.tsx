'use client'

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
