'use client'

import { Button, Card, Col, Layout, Nav, Row, Spin, Tag, Toast, Typography } from '@douyinfe/semi-ui'
import { IconRefresh, IconVideoListStroked } from '@douyinfe/semi-icons'
import useSWR from 'swr'
import { API_BASE, fetcher, ReplayStreamerState } from '@/app/lib/api-streamer'

type SubmissionPartPhase = 'recording' | 'waiting' | 'uploading' | 'completed' | 'error'

interface SubmissionPart {
  job_id?: number
  part_number: number
  phase: SubmissionPartPhase
  file_size: number
  last_error?: string
  can_retry: boolean
}

interface SubmissionSession {
  id: number
  streamer_name: string
  bvid?: string
  requires_submission_reconciliation: boolean
  last_error?: string
  parts: SubmissionPart[]
}

interface SubmissionActivity {
  sessions: SubmissionSession[]
}

const phaseLabel: Record<SubmissionPartPhase, string> = {
  recording: '录制',
  uploading: '上传',
  waiting: '等待上传',
  completed: '已完成',
  error: '异常',
}

const phaseColor: Record<SubmissionPartPhase, 'red' | 'blue' | 'grey' | 'green' | 'orange'> = {
  recording: 'red',
  uploading: 'blue',
  waiting: 'grey',
  completed: 'green',
  error: 'orange',
}

const phaseOrder: SubmissionPartPhase[] = ['error', 'recording', 'uploading', 'waiting', 'completed']

const formatBytes = (value?: number) => {
  const bytes = Math.max(0, value ?? 0)
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let current = bytes / 1024
  let index = 0
  while (current >= 1024 && index < units.length - 1) {
    current /= 1024
    index += 1
  }
  return `${current.toFixed(index >= 2 ? 2 : 1)} ${units[index]}`
}

const formatDuration = (seconds?: number) => {
  const value = Math.max(0, Math.floor(seconds ?? 0))
  const hours = Math.floor(value / 3600)
  const minutes = Math.floor((value % 3600) / 60)
  const secs = value % 60
  return [hours, minutes, secs].map(item => String(item).padStart(2, '0')).join(':')
}

const compactParts = (parts: SubmissionPart[]) => {
  const values = Array.from(new Set(parts.map(part => part.part_number))).sort((a, b) => a - b)
  if (values.length === 0) return '-'

  const ranges: string[] = []
  let start = values[0]
  let previous = values[0]
  for (let index = 1; index <= values.length; index += 1) {
    const current = values[index]
    if (current === previous + 1) {
      previous = current
      continue
    }
    ranges.push(start === previous ? `P${start}` : `P${start}–P${previous}`)
    start = current
    previous = current
  }
  return ranges.join('　')
}

export default function ReplayPage() {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const {
    data,
    error,
    isLoading,
    mutate: refresh,
  } = useSWR<SubmissionActivity>('/v1/replay/submissions', fetcher, { refreshInterval: 3000 })
  const { data: streamers, mutate: refreshStreamers } = useSWR<ReplayStreamerState[]>('/v1/replay/streamers', fetcher, { refreshInterval: 3000 })

  const retry = async (id?: number) => {
    if (!id) return
    const response = await fetch(`${API_BASE}/v1/replay/jobs/${id}/retry`, { method: 'POST' })
    if (!response.ok) {
      Toast.error(await response.text())
      return
    }
    Toast.success('已重试')
    await refresh()
  }

  const bindSubmission = async (session: SubmissionSession) => {
    const aidText = window.prompt('AID')
    if (!aidText) return
    const aid = Number(aidText.trim())
    if (!Number.isSafeInteger(aid) || aid <= 0) {
      Toast.error('AID 无效')
      return
    }
    const bvid = window.prompt('BVID（可留空）')?.trim() || undefined
    const response = await fetch(`${API_BASE}/v1/replay/sessions/${session.id}/bind-submission`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ aid, bvid }),
    })
    if (!response.ok) {
      Toast.error(await response.text())
      return
    }
    Toast.success('已绑定')
    await refresh()
  }

  const resetSubmission = async (session: SubmissionSession) => {
    const confirmed = window.confirm('确认 B 站没有生成这次投稿？')
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
    Toast.success('已重置')
    await refresh()
  }

  const refreshAll = async () => {
    await Promise.all([refresh(), refreshStreamers()])
  }

  if (isLoading) return <Spin size="large" />

  const sessions = data?.sessions ?? []
  const counts = sessions
    .flatMap(session => session.parts)
    .reduce<Record<SubmissionPartPhase, number>>((result, part) => {
      result[part.phase] += 1
      return result
    }, { recording: 0, waiting: 0, uploading: 0, completed: 0, error: 0 })

  const activeStreamerBySession = new Map<number, ReplayStreamerState>()
  for (const streamer of streamers ?? []) {
    if (streamer.active_session_id) activeStreamerBySession.set(streamer.active_session_id, streamer)
  }

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
          footer={<Button icon={<IconRefresh />} onClick={refreshAll}>刷新</Button>}
        />
      </Header>

      <Content style={{ padding: 20, backgroundColor: 'var(--semi-color-bg-0)', overflow: 'auto' }}>
        <div style={{ maxWidth: 980, margin: '0 auto' }}>
          {error && (
            <Card style={{ marginBottom: 16 }}>
              <Text type="danger">加载失败：{String(error)}</Text>
            </Card>
          )}

          <Row gutter={[12, 12]} style={{ marginBottom: 18 }}>
            <Col span={6}><Card><Title heading={4}>{counts.recording}</Title><Text type="tertiary">录制</Text></Card></Col>
            <Col span={6}><Card><Title heading={4}>{counts.uploading}</Title><Text type="tertiary">上传</Text></Card></Col>
            <Col span={6}><Card><Title heading={4}>{counts.waiting}</Title><Text type="tertiary">等待</Text></Card></Col>
            <Col span={6}><Card><Title heading={4}>{counts.error}</Title><Text type={counts.error > 0 ? 'danger' : 'tertiary'}>异常</Text></Card></Col>
          </Row>

          {sessions.length === 0 && (
            <Card style={{ textAlign: 'center', padding: 28 }}>
              <Text type="tertiary">暂无投稿</Text>
            </Card>
          )}

          <div style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
            {sessions.map(session => {
              const grouped = phaseOrder.reduce<Record<SubmissionPartPhase, SubmissionPart[]>>((result, phase) => {
                result[phase] = session.parts.filter(part => part.phase === phase)
                return result
              }, { recording: [], waiting: [], uploading: [], completed: [], error: [] })
              const activeStreamer = activeStreamerBySession.get(session.id)
              const hasError = grouped.error.length > 0 || session.requires_submission_reconciliation

              return (
                <Card
                  key={session.id}
                  style={hasError ? { borderColor: 'var(--semi-color-warning)' } : undefined}
                  title={<Title heading={5} style={{ margin: 0 }}>{session.streamer_name}</Title>}
                  headerExtraContent={session.bvid ? (
                    <a href={`https://www.bilibili.com/video/${session.bvid}`} target="_blank" rel="noreferrer">
                      {session.bvid}
                    </a>
                  ) : null}
                >
                  <div style={{ display: 'flex', flexDirection: 'column' }}>
                    {phaseOrder.map(phase => {
                      const parts = grouped[phase]
                      if (parts.length === 0) return null

                      const recordingDetail = phase === 'recording' && activeStreamer
                        ? `${formatDuration(activeStreamer.recording_elapsed_seconds)}　${formatBytes(activeStreamer.recording_bytes)}`
                        : ''
                      const uploadDetail = phase === 'uploading' && parts.length === 1
                        ? formatBytes(parts[0].file_size)
                        : ''
                      const firstError = phase === 'error' ? parts.find(part => part.last_error) : undefined
                      const errorText = firstError?.last_error || (phase === 'error' ? session.last_error : undefined)

                      return (
                        <div
                          key={phase}
                          style={{
                            minHeight: 44,
                            display: 'flex',
                            alignItems: 'center',
                            gap: 14,
                            borderTop: '1px solid var(--semi-color-border)',
                            padding: '10px 0',
                          }}
                        >
                          <div style={{ width: 82, flexShrink: 0 }}>
                            <Tag color={phaseColor[phase]}>{phaseLabel[phase]}</Tag>
                          </div>
                          <Text strong style={{ minWidth: 90 }}>{compactParts(parts)}</Text>
                          {(recordingDetail || uploadDetail) && (
                            <Text type="tertiary">{recordingDetail || uploadDetail}</Text>
                          )}
                          {errorText && (
                            <Text type="danger" ellipsis={{ showTooltip: true }} style={{ flex: 1, minWidth: 0 }}>
                              {errorText}
                            </Text>
                          )}
                          {phase === 'error' && parts.some(part => part.can_retry) && !session.requires_submission_reconciliation && (
                            <Button size="small" onClick={() => retry(parts.find(part => part.can_retry)?.job_id)}>重试</Button>
                          )}
                          {phase === 'error' && session.requires_submission_reconciliation && (
                            <>
                              <Button size="small" theme="solid" onClick={() => bindSubmission(session)}>绑定稿件</Button>
                              <Button size="small" type="danger" onClick={() => resetSubmission(session)}>确认无稿</Button>
                            </>
                          )}
                        </div>
                      )
                    })}

                    {session.requires_submission_reconciliation && grouped.error.length === 0 && (
                      <div
                        style={{
                          minHeight: 44,
                          display: 'flex',
                          alignItems: 'center',
                          gap: 10,
                          borderTop: '1px solid var(--semi-color-border)',
                          padding: '10px 0',
                        }}
                      >
                        <Tag color="orange">异常</Tag>
                        {session.last_error && (
                          <Text type="danger" ellipsis={{ showTooltip: true }} style={{ flex: 1, minWidth: 0 }}>
                            {session.last_error}
                          </Text>
                        )}
                        <Button size="small" theme="solid" onClick={() => bindSubmission(session)}>绑定稿件</Button>
                        <Button size="small" type="danger" onClick={() => resetSubmission(session)}>确认无稿</Button>
                      </div>
                    )}
                  </div>
                </Card>
              )
            })}
          </div>
        </div>
      </Content>
    </>
  )
}
