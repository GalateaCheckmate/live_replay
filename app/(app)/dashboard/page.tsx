'use client'

import React, { useRef, useState } from 'react'
import useSWR from 'swr'
import useSWRMutation from 'swr/mutation'
import {
  Avatar,
  Button,
  Card,
  Col,
  Form,
  Layout,
  Nav,
  Notification,
  Row,
  Spin,
  Toast,
  Typography,
} from '@douyinfe/semi-ui'
import { FormApi } from '@douyinfe/semi-ui/lib/es/form'
import { IconPlusCircle, IconRefresh, IconSetting } from '@douyinfe/semi-icons'
import { fetcher, put } from '@/app/lib/api-streamer'
import { useBiliUsers } from '@/app/lib/use-streamers'
import UserList from '@/app/ui/UserList'

interface DiskStatus {
  directory: string
  free_bytes?: number
  free_gb?: number
  used_bytes?: number
  used_gb?: number
  warning_gb: number
  stop_gb: number
  state: 'ok' | 'warning' | 'blocked' | 'unknown'
  message: string
}

const formatStorage = (value?: number) => {
  if (value === undefined) return '-'
  return `${value < 10 ? value.toFixed(2) : value.toFixed(1)} GB`
}

const ReplaySettings: React.FC = () => {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const { data: entity, error, isLoading, mutate: refreshConfig } = useSWR('/v1/replay/settings', fetcher)
  const { data: disk, mutate: refreshDisk } = useSWR<DiskStatus>('/v1/replay/storage', fetcher, { refreshInterval: 10000 })
  const { trigger } = useSWRMutation('/v1/replay/settings', put)
  const { biliUsers, isLoading: biliUsersLoading } = useBiliUsers()
  const formRef = useRef<FormApi>()
  const [accountManagerVisible, setAccountManagerVisible] = useState(false)

  if (isLoading) return <Spin size="large" />
  if (error) return <div style={{ padding: 24 }}>设置加载失败：{String(error)}</div>

  const save = async (values: any) => {
    try {
      const payload = { ...entity, ...values }
      if (payload.file_size === '') payload.file_size = null
      if (payload.segment_time === '') payload.segment_time = null
      await trigger(payload)
      Toast.success('设置已保存')
      await Promise.all([refreshConfig(), refreshDisk()])
    } catch (e: any) {
      Notification.error({
        title: '保存失败',
        content: <Typography.Paragraph style={{ maxWidth: 480 }}>{e?.message ?? String(e)}</Typography.Paragraph>,
        duration: 0,
      })
      throw e
    }
  }

  return (
    <>
      <Header style={{ backgroundColor: 'var(--semi-color-bg-1)' }}>
        <Nav
          header={
            <>
              <div style={{ backgroundColor: '#6b6c75ff', borderRadius: 8, color: 'white', display: 'flex', padding: 6 }}>
                <IconSetting size="large" />
              </div>
              <h4 style={{ marginLeft: 12 }}>设置</h4>
            </>
          }
          footer={
            <Button icon={<IconRefresh />} onClick={() => Promise.all([refreshConfig(), refreshDisk()])}>
              刷新
            </Button>
          }
          mode="horizontal"
        />
      </Header>

      <Content style={{ padding: 24, backgroundColor: 'var(--semi-color-bg-0)', overflow: 'auto' }}>
        <div style={{ maxWidth: 980, margin: '0 auto' }}>
          <Card
            title="B站投稿账号"
            style={{ marginBottom: 16 }}
            headerExtraContent={
              <Button
                icon={<IconPlusCircle />}
                theme="solid"
                onClick={() => setAccountManagerVisible(true)}
              >
                管理账号
              </Button>
            }
          >
            {biliUsersLoading ? (
              <Spin />
            ) : (biliUsers ?? []).length === 0 ? (
              <Text strong>暂无投稿账号</Text>
            ) : (
              <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap' }}>
                {(biliUsers ?? []).map(account => (
                  <div
                    key={account.id}
                    style={{
                      display: 'flex',
                      alignItems: 'center',
                      gap: 10,
                      padding: '8px 12px',
                      border: '1px solid var(--semi-color-border)',
                      borderRadius: 8,
                      backgroundColor: 'var(--semi-color-bg-1)',
                    }}
                  >
                    <Avatar size="small" src={account.face}>{account.name?.slice(0, 1)}</Avatar>
                    <Text strong>{account.name}</Text>
                  </div>
                ))}
              </div>
            )}
          </Card>

          {disk && (
            <Card style={{ marginBottom: 16 }}>
              <Row gutter={16} align="middle">
                <Col span={12}>
                  <Title heading={5}>存储</Title>
                  <Text type="tertiary">录像目录：{disk.directory}</Text>
                  {disk.state !== 'ok' && (
                    <><br /><Text type={disk.state === 'warning' ? 'warning' : 'danger'}>{disk.message}</Text></>
                  )}
                </Col>
                <Col span={6} style={{ textAlign: 'right' }}>
                  <Title heading={4}>{formatStorage(disk.used_gb)}</Title>
                  <Text type="tertiary">录像占用</Text>
                </Col>
                <Col span={6} style={{ textAlign: 'right' }}>
                  <Title heading={4}>{formatStorage(disk.free_gb)}</Title>
                  <Text type="tertiary">剩余空间</Text>
                </Col>
              </Row>
            </Card>
          )}

          <Form initValues={entity} getFormApi={api => (formRef.current = api)} onSubmit={save}>
            <Card title="录制" style={{ marginBottom: 16 }}>
              <Form.Select
                field="downloader"
                label="录制引擎"
                optionList={[
                  { label: 'Stream Gears（推荐）', value: 'stream-gears' },
                  { label: 'FFmpeg', value: 'ffmpeg' },
                ]}
                placeholder="Stream Gears（推荐）"
                style={{ width: '100%' }}
              />
              <Form.Input
                field="segment_time"
                label="默认分段时长"
                placeholder="01:00:00"
                extraText="格式：时:分:秒。主播单独设置的分段时长会覆盖这里。"
                rules={[
                  { pattern: /^[^：]*$/, message: '请使用英文冒号' },
                  { pattern: /^$|^[0-9]{2,4}:[0-5][0-9]:[0-5][0-9]$/, message: '格式应为 01:00:00' },
                ]}
              />
              <Form.Input
                field="filename_prefix"
                label="默认文件名"
                placeholder="{streamer}%Y-%m-%dT%H_%M_%S"
                extraText="支持 {streamer}、{title} 和日期时间格式。"
              />
              <Form.InputNumber
                field="event_loop_interval"
                label="开播检测间隔"
                suffix="秒"
                min={1}
                style={{ width: '100%' }}
              />
              <Form.InputNumber
                field="pool1_size"
                label="最大同时录制数"
                min={1}
                style={{ width: '100%' }}
              />
            </Card>

            <Card title="后台上传" style={{ marginBottom: 16 }}>
              <Form.InputNumber
                field="pool2_size"
                label="最大同时上传文件数"
                min={1}
                max={16}
                style={{ width: '100%' }}
                extraText="控制同时上传的录像文件数量。修改后立即生效，不会中断正在上传的文件。"
              />
              <Form.Select
                field="lines"
                label="B站上传线路"
                optionList={[
                  { label: 'AUTO（自动）', value: 'AUTO' },
                  { label: 'bda2（百度云）', value: 'bda2' },
                  { label: 'bldsa（B站）', value: 'bldsa' },
                  { label: 'tx（腾讯云）', value: 'tx' },
                  { label: 'txa（海外腾讯云）', value: 'txa' },
                  { label: 'alia（海外阿里云）', value: 'alia' },
                ]}
                style={{ width: '100%' }}
              />
              <Form.InputNumber
                field="threads"
                label="单文件上传并发"
                min={1}
                max={8}
                style={{ width: '100%' }}
                extraText="较高并发可能提升上传速度，也会占用更多带宽。"
              />
              <Form.Select
                field="submit_api"
                label="投稿接口"
                optionList={[
                  { label: 'APP（默认）', value: 'app' },
                  { label: 'Web', value: 'web' },
                ]}
                showClear
                style={{ width: '100%' }}
              />
            </Card>

            <Card title="抖音直播源" style={{ marginBottom: 16 }}>
              <Form.Select
                field="douyin_quality"
                label="画质"
                optionList={[
                  { label: '原画', value: 'origin' },
                  { label: '蓝光', value: 'uhd' },
                  { label: '超清', value: 'hd' },
                  { label: '高清', value: 'sd' },
                ]}
                showClear
                style={{ width: '100%' }}
              />
              <Form.Select
                field="douyin_protocol"
                label="直播协议"
                optionList={[
                  { label: 'FLV（推荐）', value: 'flv' },
                  { label: 'HLS', value: 'hls' },
                ]}
                showClear
                style={{ width: '100%' }}
              />
              <Form.Switch field="douyin_true_origin" label="优先真原画" />
            </Card>

            <Card title="B站直播源" style={{ marginBottom: 16 }}>
              <Form.Select
                field="bili_protocol"
                label="直播协议"
                optionList={[
                  { label: 'Stream（推荐）', value: 'stream' },
                  { label: 'HLS TS', value: 'hls_ts' },
                  { label: 'HLS fMP4', value: 'hls_fmp4' },
                ]}
                showClear
                style={{ width: '100%' }}
              />
              <Form.InputNumber field="bili_qn" label="画质编号" min={0} style={{ width: '100%' }} />
              <Form.Switch field="bili_cdn_fallback" label="CDN 自动回退" />
            </Card>

            <Card>
              <Button theme="solid" onClick={() => formRef.current?.submitForm()}>保存设置</Button>
            </Card>
          </Form>
        </div>
      </Content>

      <UserList
        visible={accountManagerVisible}
        onCancel={() => setAccountManagerVisible(false)}
      />
    </>
  )
}

export default ReplaySettings
