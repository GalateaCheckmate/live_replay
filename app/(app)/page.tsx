'use client'

import { useMemo, useState } from 'react'
import Link from 'next/link'
import useSWR from 'swr'
import { Button, Card, Col, Form, Layout, Modal, Notification, Row, Switch, Tag, Typography } from '@douyinfe/semi-ui'
import { IconPlusCircle, IconRefresh } from '@douyinfe/semi-icons'
import { useSWRConfig } from 'swr'
import useStreamers, { useBiliUsers, useTypeTree } from '../lib/use-streamers'
import { API_BASE, fetcher } from '../lib/api-streamer'

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
  const { typeTree } = useTypeTree()
  const { data: diskStatus, mutate: refreshDisk } = useSWR<DiskStatus>('/v1/disk-status', fetcher, { refreshInterval: 10000 })
  const { mutate } = useSWRConfig()
  const [visible, setVisible] = useState(false)
  const [saving, setSaving] = useState(false)
  const [formApi, setFormApi] = useState<any>()
  const [finalizing, setFinalizing] = useState<Set<number>>(new Set())

  const deltaForceTid = useMemo(() => {
    const children = (typeTree ?? []).flatMap((item: any) => item.children ?? [])
    return children.find((item: any) => item.name?.trim() === '三角洲行动')?.id as number | undefined
  }, [typeTree])

  const accountOptions = (biliUsers ?? []).map(item => ({ label: item.name, value: item.value }))

  const createStreamer = async () => {
    if (deltaForceTid === undefined) {
      Notification.error({ title: '无法添加', content: '未能从B站获取“三角洲行动”分区，请刷新后重试。不会自动改投其他分区。' })
      return
    }
    const values = await formApi?.validate()
    if (!values) return
    setSaving(true)
    try {
      const response = await fetch(`${API_BASE}/v1/streamers/simple`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...values, tid: deltaForceTid }),
      })
      if (!response.ok) throw new Error(await response.text())
      Notification.success({ title: '添加成功', content: '已持续关注；开播后会自动录制、上传并在可播放后删除本地视频。' })
      setVisible(false)
      formApi?.reset()
      await mutate('/v1/streamers')
    } catch (error: any) {
      Notification.error({ title: '添加失败', content: error.message })
    } finally {
      setSaving(false)
    }
  }

  const toggleStreamer = async (id: number) => {
    const streamer = streamers?.find(item => item.id === id)
    const isDisabling = streamer?.enabled !== false
    if (isDisabling) {
      setFinalizing(previous => new Set(previous).add(id))
    }
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
            <Button theme="solid" icon={<IconPlusCircle />} onClick={() => setVisible(true)}>添加主播</Button>
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
            <div style={{ marginTop: 20 }}><Button theme="solid" onClick={() => setVisible(true)}>添加第一个主播</Button></div>
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
                    <Text>自动行为：录制 → 上传 → B站可播放 → 删除本地视频</Text>
                    <Text>投稿默认：三角洲行动 · 仅自己可见 · 转载</Text>
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
      >
        <Form getFormApi={setFormApi} initValues={{ user_cookie: accountOptions[0]?.value }}>
          <Form.Input field="url" label="直播间链接" placeholder="粘贴抖音、B站或斗鱼直播间链接" rules={[{ required: true, message: '请填写直播间链接' }]} />
          <Form.Input field="remark" label="主播名称" placeholder="例如：小天才" rules={[{ required: true, message: '请填写主播名称' }]} />
          <Form.Select field="user_cookie" label="投稿账号" optionList={accountOptions} rules={[{ required: true, message: '请先登录B站账号' }]} style={{ width: '100%' }} />
          <Card style={{ marginTop: 12 }}>
            <Text>默认标题：主播名 直播回放 日期 时间</Text><br />
            <Text>分区/标签：三角洲行动 / 游戏</Text><br />
            <Text>可见范围：仅自己可见　类型：转载</Text><br />
            <Text>简介：空　分段：60分钟　弹幕：不录制</Text><br />
            <Text>磁盘：低于30GB提醒，低于10GB停止新录制</Text>
          </Card>
          {accountOptions.length === 0 && <Typography.Text type="danger">当前没有可用B站账号，请先扫码登录。</Typography.Text>}
          {deltaForceTid === undefined && <Typography.Text type="danger">未找到B站“三角洲行动”分区，当前不能添加主播。</Typography.Text>}
        </Form>
      </Modal>
    </>
  )
}
