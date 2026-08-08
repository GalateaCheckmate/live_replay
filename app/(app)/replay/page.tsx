'use client'

import { Button, Card, Col, Layout, Nav, Row, Spin, Table, Tag, Toast, Typography } from '@douyinfe/semi-ui'
import { IconRefresh, IconVideoListStroked } from '@douyinfe/semi-icons'
import useSWR from 'swr'
import { API_BASE, fetcher, ReplayUserState } from '@/app/lib/api-streamer'

interface ReplaySessionSummary {
  id: number
  streamer_name: string
  live_title: string
  started_at: string
  ended_at?: string
  user_state: ReplayUserState
  completed: boolean
  expected_parts: number
  verified_parts: number
  pending_parts: number
  bvid?: string
  last_error?: string
  requires_submission_reconciliation: boolean
}

interface ReplaySegmentSummary {
  job_id: number
  session_id: number
  streamer_name: string
  part_number: number
  user_state: ReplayUserState
  completed: boolean
  file_size: number
  attempts: number
  last_error?: string
  next_attempt_at?: string
  can_retry: boolean
}

interface ReplayActivity {
  sessions: ReplaySessionSummary[]
  segments: ReplaySegmentSummary[]
}

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

const formatBytes = (value: number) => {
  if (!value) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1)
  return `${(value / Math.pow(1024, index)).toFixed(index >= 3 ? 2 : 1)} ${units[index]}`
}

const formatTime = (value?: string) => {
  if (!value) return '-'
  const normalized = /(?:Z|[+-]\d{2}:\d{2})$/.test(value) ? value : `${value}Z`
  return new Date(normalized).toLocaleString()
}

export default function ReplayPage() {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const {
    data,
    error,
    isLoading,
    mutate: refresh,
  } = useSWR<ReplayActivity>('/v1/replay/activity', fetcher, { refreshInterval: 3000 })

  const retry = async (id: number) => {
    const response = await fetch(`${API_BASE}/v1/replay/jobs/${id}/retry`, { method: 'POST' })
    if (!response.ok) {
      Toast.error(await response.text())
      return
    }
    Toast.success('已安排重新处理')
    await refresh()
  }

  const bindSubmission = async (session: ReplaySessionSummary) => {
    const aidText = window.prompt('请输入已经存在的稿件 AID（纯数字）')
    if (!aidText) return
    const aid = Number(aidText.trim())
    if (!Number.isSafeInteger(aid) || aid <= 0) {
      Toast.error('AID 格式不正确')
      return
    }
    const bvid = window.prompt('请输入 BVID；不知道可以留空')?.trim() || undefined
    const response = await fetch(`${API_BASE}/v1/replay/sessions/${session.id}/bind-submission`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ aid, bvid }),
    })
    if (!response.ok) {
      Toast.error(await response.text())
      return
    }
    Toast.success('稿件已绑定，后续分P将继续处理')
    await refresh()
  }

  const resetSubmission = async (session: ReplaySessionSummary) => {
    const confirmed = window.confirm(
      '只有确认 B 站创作中心没有生成这场直播的稿件时，才能重新创建投稿。\n\n确认远端没有稿件并继续吗？'
    )
    if (!confirmed) return
    const response = await fetch(`${API_BASE}/v1/replay/sessions/${session.id}/reset-submission`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ confirm_no_remote_submission: true }),
    })
    if (!response.ok) {
      Toast.error(await response.text())
      return
    }
    Toast.success('已确认远端无稿，将重新创建投稿')
    await refresh()
  }

  if (isLoading) return <Spin size="large" />

  const sessions = data?.sessions ?? []
  const activeSessions = sessions.filter(item => !item.completed)
  const activeSegments = (data?.segments ?? []).filter(item => !item.completed)
  const errorSessions = activeSessions.filter(item => item.user_state === 'error').length
  const pendingBytes = activeSegments.reduce((sum, item) => sum + item.file_size, 0)

  const sessionColumns = [
    { title: '主播', dataIndex: 'streamer_name', width: 130 },
    { title: '开始时间', dataIndex: 'started_at', width: 180, render: formatTime },
    {
      title: '状态',
      width: 110,
      render: (_: unknown, record: ReplaySessionSummary) => record.completed
        ? <Tag color="green">已完成</Tag>
        : <Tag color={stateColor[record.user_state]}>{stateText[record.user_state]}</Tag>,
    },
    {
      title: '分P',
      width: 100,
      render: (_: unknown, record: ReplaySessionSummary) => `${record.verified_parts}/${record.expected_parts}`,
    },
    {
      title: 'B站投稿',
      width: 180,
      render: (_: unknown, record: ReplaySessionSummary) => record.bvid
        ? <a href={`https://www.bilibili.com/video/${record.bvid}`} target="_blank" rel="noreferrer">{record.bvid}</a>
        : <Text type="tertiary">尚未创建</Text>,
    },
    {
      title: '说明',
      render: (_: unknown, record: ReplaySessionSummary) => {
        if (record.user_state === 'error') {
          return <Text type="danger" ellipsis={{ showTooltip: true }}>{record.last_error || '投稿结果需要人工确认'}</Text>
        }
        if (record.completed) return <Text type="tertiary">本场直播处理完成。</Text>
        if (record.user_state === 'recording') return <Text>直播仍在进行，已完成的录像分段会自动投稿。</Text>
        return <Text>还有 {record.pending_parts} 个录像分段正在处理。</Text>
      },
    },
    {
      title: '操作',
      width: 190,
      render: (_: unknown, record: ReplaySessionSummary) => record.requires_submission_reconciliation ? (
        <div style={{ display: 'flex', gap: 6 }}>
          <Button size="small" theme="solid" onClick={() => bindSubmission(record)}>绑定稿件</Button>
          <Button size="small" type="danger" onClick={() => resetSubmission(record)}>确认无稿</Button>
        </div>
      ) : '-',
    },
  ]

  const segmentColumns = [
    { title: '主播', dataIndex: 'streamer_name', width: 120 },
    { title: '分P', dataIndex: 'part_number', width: 70, render: (value: number) => `P${value}` },
    {
      title: '状态',
      width: 110,
      render: (_: unknown, record: ReplaySegmentSummary) => (
        <Tag color={stateColor[record.user_state]}>{stateText[record.user_state]}</Tag>
      ),
    },
    { title: '文件大小', dataIndex: 'file_size', width: 110, render: formatBytes },
    { title: '处理次数', dataIndex: 'attempts', width: 90 },
    {
      title: '下次处理',
      dataIndex: 'next_attempt_at',
      width: 180,
      render: (value?: string) => value ? formatTime(value) : '自动',
    },
    {
      title: '说明',
      render: (_: unknown, record: ReplaySegmentSummary) => record.last_error
        ? <Text type={record.user_state === 'error' ? 'danger' : 'tertiary'} ellipsis={{ showTooltip: true }}>{record.last_error}</Text>
        : <Text type="tertiary">自动处理</Text>,
    },
    {
      title: '操作',
      width: 90,
      render: (_: unknown, record: ReplaySegmentSummary) => (
        <Button size="small" disabled={!record.can_retry} onClick={() => retry(record.job_id)}>重试</Button>
      ),
    },
  ]

  return (
    <>
      <Header style={{ backgroundColor: 'var(--semi-color-bg-1)' }}>
        <Nav
          style={{ border: 'none' }}
          header={
            <>
              <div style={{ backgroundColor: 'rgb(250 102 76)', borderRadius: 8, color: 'white', display: 'flex', padding: 6 }}>
                <IconVideoListStroked size="large" />
              </div>
              <h4 style={{ marginLeft: 12 }}>投稿</h4>
            </>
          }
          mode="horizontal"
          footer={<Button icon={<IconRefresh />} onClick={() => refresh()}>刷新</Button>}
        />
      </Header>
      <Content style={{ padding: 16, backgroundColor: 'var(--semi-color-bg-0)', overflow: 'auto' }}>
        {error && (
          <Card style={{ marginBottom: 16 }}>
            <Text type="danger">投稿状态加载失败：{String(error)}</Text>
          </Card>
        )}

        <Row gutter={[12, 12]} style={{ marginBottom: 16 }}>
          <Col span={8}><Card><Title heading={4}>{activeSessions.length}</Title><Text>正在处理的直播场次</Text></Card></Col>
          <Col span={8}><Card><Title heading={4}>{errorSessions}</Title><Text>需要人工处理</Text></Card></Col>
          <Col span={8}><Card><Title heading={4}>{formatBytes(pendingBytes)}</Title><Text>待处理本地录像</Text></Card></Col>
        </Row>

        <Title heading={5} style={{ marginBottom: 10 }}>直播场次</Title>
        <Table<ReplaySessionSummary>
          size="small"
          rowKey="id"
          columns={sessionColumns}
          dataSource={sessions}
          pagination={{ pageSize: 12 }}
          empty="暂无投稿记录"
          style={{ marginBottom: 24 }}
        />

        <Title heading={5} style={{ marginBottom: 4 }}>待处理分段</Title>
        <Text type="tertiary">显示尚未完成的录像分段，完成后会自动从这里移除。</Text>
        <Table<ReplaySegmentSummary>
          size="small"
          rowKey="job_id"
          columns={segmentColumns}
          dataSource={activeSegments}
          pagination={{ pageSize: 20 }}
          empty="暂无待处理分段"
          style={{ marginTop: 10 }}
        />
      </Content>
    </>
  )
}
