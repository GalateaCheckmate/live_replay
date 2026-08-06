'use client'

import { Button, Card, Col, Layout, Nav, Row, Spin, Table, Tag, Toast, Typography } from '@douyinfe/semi-ui'
import { IconHistory, IconRefresh } from '@douyinfe/semi-icons'
import useSWR from 'swr'
import { API_BASE, fetcher } from '@/app/lib/api-streamer'

interface ReplaySession {
  id: number
  live_streamer_id: number
  streamer_name: string
  streamer_url: string
  live_title: string
  started_at: string
  ended_at?: string
  status: string
  aid?: number
  bvid?: string
  expected_parts: number
  verified_parts: number
  last_error?: string
  pending_parts: number
}

interface ReplayJob {
  id: number
  session_id: number
  segment_id: number
  streamer_name: string
  bvid?: string
  part_number: number
  file_path: string
  file_size: number
  segment_status: string
  job_status: string
  attempts: number
  last_error?: string
  next_attempt_at?: string
  uploaded_at?: string
  verified_at?: string
  deleted_at?: string
}

const statusColor = (status: string) => {
  if (['complete', 'deleted', 'verified'].includes(status)) return 'green'
  if (['uploading', 'recording'].includes(status)) return 'blue'
  if (['queued', 'recording_complete'].includes(status)) return 'cyan'
  if (['retry_wait', 'retrying'].includes(status)) return 'orange'
  return 'red'
}

const formatBytes = (value: number) => {
  if (!value) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1)
  return `${(value / Math.pow(1024, index)).toFixed(index >= 3 ? 2 : 1)} ${units[index]}`
}

export default function ReplayPage() {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const {
    data: sessions,
    isLoading: sessionsLoading,
    mutate: refreshSessions,
  } = useSWR<ReplaySession[]>('/v1/replay/sessions', fetcher, { refreshInterval: 3000 })
  const {
    data: jobs,
    isLoading: jobsLoading,
    mutate: refreshJobs,
  } = useSWR<ReplayJob[]>('/v1/replay/jobs', fetcher, { refreshInterval: 3000 })

  const retry = async (id: number) => {
    const response = await fetch(`${API_BASE}/v1/replay/jobs/${id}/retry`, { method: 'POST' })
    if (!response.ok) {
      Toast.error(await response.text())
      return
    }
    Toast.success('已解除等待，任务会尽快重试')
    await Promise.all([refreshJobs(), refreshSessions()])
  }

  if (sessionsLoading || jobsLoading) return <Spin size="large" />

  const activeSessions = sessions?.filter(item => item.status !== 'complete').length ?? 0
  const queuedJobs = jobs?.filter(item => ['queued', 'uploading', 'retry_wait'].includes(item.job_status)).length ?? 0
  const failedJobs = jobs?.filter(item => item.last_error).length ?? 0
  const pendingBytes = jobs
    ?.filter(item => !item.deleted_at)
    .reduce((sum, item) => sum + item.file_size, 0) ?? 0

  const sessionColumns = [
    { title: '主播', dataIndex: 'streamer_name', width: 130 },
    { title: '直播标题', dataIndex: 'live_title' },
    {
      title: '场次状态',
      dataIndex: 'status',
      width: 130,
      render: (value: string) => <Tag color={statusColor(value)}>{value}</Tag>,
    },
    {
      title: '分P进度',
      width: 110,
      render: (_: unknown, record: ReplaySession) => `${record.verified_parts}/${record.expected_parts}`,
    },
    {
      title: '投稿',
      dataIndex: 'bvid',
      width: 150,
      render: (value?: string) => value
        ? <a href={`https://www.bilibili.com/video/${value}`} target="_blank" rel="noreferrer">{value}</a>
        : '尚未创建',
    },
    {
      title: '开始时间',
      dataIndex: 'started_at',
      width: 190,
      render: (value: string) => new Date(value).toLocaleString(),
    },
    {
      title: '最近错误',
      dataIndex: 'last_error',
      render: (value?: string) => value ? <Text type="danger" ellipsis={{ showTooltip: true }}>{value}</Text> : '-',
    },
  ]

  const jobColumns = [
    { title: '主播', dataIndex: 'streamer_name', width: 120 },
    { title: '分P', dataIndex: 'part_number', width: 65, render: (value: number) => `P${value}` },
    {
      title: '状态',
      dataIndex: 'job_status',
      width: 110,
      render: (value: string) => <Tag color={statusColor(value)}>{value}</Tag>,
    },
    { title: '文件大小', dataIndex: 'file_size', width: 105, render: formatBytes },
    { title: '尝试次数', dataIndex: 'attempts', width: 90 },
    {
      title: '本地文件',
      dataIndex: 'file_path',
      render: (value: string) => <Text ellipsis={{ showTooltip: true }}>{value}</Text>,
    },
    {
      title: '下次重试',
      dataIndex: 'next_attempt_at',
      width: 180,
      render: (value?: string) => value ? new Date(`${value}Z`).toLocaleString() : '-',
    },
    {
      title: '失败原因',
      dataIndex: 'last_error',
      render: (value?: string) => value ? <Text type="danger" ellipsis={{ showTooltip: true }}>{value}</Text> : '-',
    },
    {
      title: '操作',
      width: 90,
      render: (_: unknown, record: ReplayJob) => (
        <Button
          size="small"
          disabled={['complete', 'verified'].includes(record.job_status)}
          onClick={() => retry(record.id)}
        >
          重试
        </Button>
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
                <IconHistory size="large" />
              </div>
              <h4 style={{ marginLeft: 12 }}>Live Replay 上传队列</h4>
            </>
          }
          mode="horizontal"
          footer={
            <Button
              icon={<IconRefresh />}
              onClick={() => Promise.all([refreshSessions(), refreshJobs()])}
            >
              刷新
            </Button>
          }
        />
      </Header>
      <Content style={{ padding: 16, backgroundColor: 'var(--semi-color-bg-0)' }}>
        <Row gutter={[12, 12]} style={{ marginBottom: 16 }}>
          <Col span={6}><Card><Title heading={4}>{activeSessions}</Title><Text>进行中场次</Text></Card></Col>
          <Col span={6}><Card><Title heading={4}>{queuedJobs}</Title><Text>等待/上传/重试</Text></Card></Col>
          <Col span={6}><Card><Title heading={4}>{failedJobs}</Title><Text>有错误记录的任务</Text></Card></Col>
          <Col span={6}><Card><Title heading={4}>{formatBytes(pendingBytes)}</Title><Text>尚未安全删除</Text></Card></Col>
        </Row>

        <Title heading={5} style={{ marginBottom: 10 }}>直播场次</Title>
        <Table<ReplaySession>
          size="small"
          rowKey="id"
          columns={sessionColumns}
          dataSource={sessions ?? []}
          pagination={{ pageSize: 10 }}
          style={{ marginBottom: 24 }}
        />

        <Title heading={5} style={{ marginBottom: 10 }}>分段上传任务</Title>
        <Table<ReplayJob>
          size="small"
          rowKey="id"
          columns={jobColumns}
          dataSource={jobs ?? []}
          pagination={{ pageSize: 20 }}
        />
      </Content>
    </>
  )
}
