'use client'

import { Button, Card, Col, Layout, Nav, Row, Spin, Table, Tag, Toast, Typography } from '@douyinfe/semi-ui'
import { IconRefresh, IconVideoListStroked } from '@douyinfe/semi-icons'
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
  submit_state: string
  aid?: number
  bvid?: string
  expected_parts: number
  verified_parts: number
  next_part_to_upload: number
  delete_after_success: boolean
  preserve_danmaku: boolean
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
  deleted_at?: string
}

type VisiblePhase = 'recording' | 'uploading' | 'error' | 'complete'

const phaseText: Record<VisiblePhase, string> = {
  recording: '正在录制',
  uploading: '正在上传',
  error: '异常',
  complete: '已完成',
}

const phaseColor: Record<VisiblePhase, 'red' | 'blue' | 'orange' | 'green'> = {
  recording: 'red',
  uploading: 'blue',
  error: 'orange',
  complete: 'green',
}

const manualErrorStates = new Set(['conflict', 'submission_uncertain'])

function sessionPhase(session: ReplaySession): VisiblePhase {
  if (manualErrorStates.has(session.status) || manualErrorStates.has(session.submit_state)) return 'error'
  if (session.status === 'recording' && !session.ended_at) return 'recording'
  if (session.status === 'complete' && session.pending_parts === 0) return 'complete'
  return 'uploading'
}

function jobPhase(job: ReplayJob): VisiblePhase {
  if (manualErrorStates.has(job.job_status) || manualErrorStates.has(job.segment_status)) return 'error'
  if (job.job_status === 'complete') return 'complete'
  return 'uploading'
}

const formatBytes = (value: number) => {
  if (!value) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1)
  return `${(value / Math.pow(1024, index)).toFixed(index >= 3 ? 2 : 1)} ${units[index]}`
}

const formatTime = (value?: string) => value ? new Date(value.endsWith('Z') ? value : `${value}Z`).toLocaleString() : '-'

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

  const refreshAll = () => Promise.all([refreshJobs(), refreshSessions()])

  const retry = async (id: number) => {
    const response = await fetch(`${API_BASE}/v1/replay/jobs/${id}/retry`, { method: 'POST' })
    if (!response.ok) {
      Toast.error(await response.text())
      return
    }
    Toast.success('已重新加入上传队列')
    await refreshAll()
  }

  const bindSubmission = async (session: ReplaySession) => {
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
    Toast.success('已绑定稿件，后续分P会继续按顺序处理')
    await refreshAll()
  }

  const resetSubmission = async (session: ReplaySession) => {
    const confirmed = window.confirm(
      '只有确认B站创作中心完全没有生成这场直播的稿件时才能继续。\n\n确认远端没有稿件并重新创建吗？'
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
    await refreshAll()
  }

  if (sessionsLoading || jobsLoading) return <Spin size="large" />

  const activeSessions = (sessions ?? []).filter(item => sessionPhase(item) !== 'complete')
  const activeJobs = (jobs ?? []).filter(item => jobPhase(item) !== 'complete')
  const errorSessions = activeSessions.filter(item => sessionPhase(item) === 'error').length
  const pendingBytes = activeJobs.reduce((sum, item) => sum + item.file_size, 0)

  const sessionColumns = [
    { title: '主播', dataIndex: 'streamer_name', width: 130 },
    { title: '开始时间', dataIndex: 'started_at', width: 180, render: formatTime },
    {
      title: '状态',
      width: 110,
      render: (_: unknown, record: ReplaySession) => {
        const phase = sessionPhase(record)
        return <Tag color={phaseColor[phase]}>{phaseText[phase]}</Tag>
      },
    },
    {
      title: '分P',
      width: 120,
      render: (_: unknown, record: ReplaySession) => `${record.verified_parts}/${record.expected_parts}`,
    },
    {
      title: 'B站投稿',
      width: 180,
      render: (_: unknown, record: ReplaySession) => record.bvid
        ? <a href={`https://www.bilibili.com/video/${record.bvid}`} target="_blank" rel="noreferrer">{record.bvid}</a>
        : <Text type="tertiary">等待首个分P</Text>,
    },
    {
      title: '说明',
      render: (_: unknown, record: ReplaySession) => {
        if (sessionPhase(record) === 'error') {
          return <Text type="danger" ellipsis={{ showTooltip: true }}>{record.last_error || '投稿结果需要人工确认'}</Text>
        }
        if (sessionPhase(record) === 'recording') return <Text>直播仍在进行，完成的分段会进入后台上传。</Text>
        if (sessionPhase(record) === 'uploading') return <Text>还有 {record.pending_parts} 个分段待上传、远端确认或本地清理。</Text>
        return <Text type="tertiary">本场直播已经处理完成。</Text>
      },
    },
    {
      title: '操作',
      width: 190,
      render: (_: unknown, record: ReplaySession) => record.submit_state === 'uncertain' ? (
        <div style={{ display: 'flex', gap: 6 }}>
          <Button size="small" theme="solid" onClick={() => bindSubmission(record)}>绑定稿件</Button>
          <Button size="small" type="danger" onClick={() => resetSubmission(record)}>确认无稿</Button>
        </div>
      ) : '-',
    },
  ]

  const jobColumns = [
    { title: '主播', dataIndex: 'streamer_name', width: 120 },
    { title: '分P', dataIndex: 'part_number', width: 70, render: (value: number) => `P${value}` },
    {
      title: '状态',
      width: 110,
      render: (_: unknown, record: ReplayJob) => {
        const phase = jobPhase(record)
        return <Tag color={phaseColor[phase]}>{phaseText[phase]}</Tag>
      },
    },
    { title: '文件大小', dataIndex: 'file_size', width: 110, render: formatBytes },
    {
      title: '下次处理',
      dataIndex: 'next_attempt_at',
      width: 180,
      render: (value?: string) => value ? formatTime(value) : '自动',
    },
    {
      title: '说明',
      render: (_: unknown, record: ReplayJob) => record.last_error
        ? <Text type={jobPhase(record) === 'error' ? 'danger' : 'tertiary'} ellipsis={{ showTooltip: true }}>{record.last_error}</Text>
        : <Text type="tertiary">后台自动处理</Text>,
    },
    {
      title: '操作',
      width: 90,
      render: (_: unknown, record: ReplayJob) => (
        <Button
          size="small"
          disabled={record.job_status === 'submission_uncertain'}
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
                <IconVideoListStroked size="large" />
              </div>
              <h4 style={{ marginLeft: 12 }}>场次与投稿</h4>
            </>
          }
          mode="horizontal"
          footer={<Button icon={<IconRefresh />} onClick={refreshAll}>刷新</Button>}
        />
      </Header>
      <Content style={{ padding: 16, backgroundColor: 'var(--semi-color-bg-0)' }}>
        <Row gutter={[12, 12]} style={{ marginBottom: 16 }}>
          <Col span={8}><Card><Title heading={4}>{activeSessions.length}</Title><Text>正在处理的直播场次</Text></Card></Col>
          <Col span={8}><Card><Title heading={4}>{errorSessions}</Title><Text>需要人工处理</Text></Card></Col>
          <Col span={8}><Card><Title heading={4}>{formatBytes(pendingBytes)}</Title><Text>尚未处理完成的本地录像</Text></Card></Col>
        </Row>

        <Title heading={5} style={{ marginBottom: 10 }}>直播场次</Title>
        <Table<ReplaySession>
          size="small"
          rowKey="id"
          columns={sessionColumns}
          dataSource={sessions ?? []}
          pagination={{ pageSize: 12 }}
          style={{ marginBottom: 24 }}
        />

        <Title heading={5} style={{ marginBottom: 4 }}>当前分段</Title>
        <Text type="tertiary">只显示尚未完成的分段；底层上传、远端转码检查和本地清理由系统自动处理。</Text>
        <Table<ReplayJob>
          size="small"
          rowKey="id"
          columns={jobColumns}
          dataSource={activeJobs}
          pagination={{ pageSize: 20 }}
          style={{ marginTop: 10 }}
        />
      </Content>
    </>
  )
}
