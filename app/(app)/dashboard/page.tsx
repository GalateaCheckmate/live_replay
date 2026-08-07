'use client'

import React, { useRef } from 'react'
import useSWR from 'swr'
import useSWRMutation from 'swr/mutation'
import {
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
import { IconRefresh, IconSetting } from '@douyinfe/semi-icons'
import { fetcher, put } from '@/app/lib/api-streamer'

interface DiskStatus {
  directory: string
  free_bytes?: number
  free_gb?: number
  warning_gb: number
  stop_gb: number
  state: 'ok' | 'warning' | 'blocked' | 'unknown'
  message: string
}

const ReplaySettings: React.FC = () => {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const { data: entity, error, isLoading, mutate: refreshConfig } = useSWR('/v1/configuration', fetcher)
  const { data: disk, mutate: refreshDisk } = useSWR<DiskStatus>('/v1/disk-status', fetcher, { refreshInterval: 10000 })
  const { trigger } = useSWRMutation('/v1/configuration', put)
  const formRef = useRef<FormApi>()

  if (isLoading) return <Spin size="large" />
  if (error) return <div style={{ padding: 24 }}>设置加载失败：{String(error)}</div>

  const save = async (values: any) => {
    try {
      // 只简化 UI，不破坏旧数据。页面未展示的兼容字段原样保留，后续真正删除
      // biliup 配置字段时再通过显式 migration 处理。
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
              <h4 style={{ marginLeft: 12 }}>Live Replay 设置</h4>
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
          {disk && (
            <Card style={{ marginBottom: 16 }}>
              <Row gutter={16}>
                <Col span={16}>
                  <Title heading={5}>存储</Title>
                  <Text>{disk.message}</Text><br />
                  <Text type="tertiary">当前录像目录：{disk.directory}</Text>
                </Col>
                <Col span={8} style={{ textAlign: 'right' }}>
                  <Title heading={4}>{disk.free_gb === undefined ? '-' : `${disk.free_gb.toFixed(1)} GB`}</Title>
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
                extraText="格式：时:分:秒。单个主播创建时设置的分段时长会覆盖这里。"
                rules={[
                  { pattern: /^[^：]*$/, message: '请使用英文冒号' },
                  { pattern: /^$|^[0-9]{2,4}:[0-5][0-9]:[0-5][0-9]$/, message: '格式应为 01:00:00' },
                ]}
              />
              <Form.Input
                field="filename_prefix"
                label="默认文件名"
                placeholder="{streamer}%Y-%m-%dT%H_%M_%S"
                extraText="建议保留 {streamer}，方便异常恢复时辨认文件。"
              />
              <Form.InputNumber
                field="filtering_threshold"
                label="碎片过滤阈值"
                suffix="MB"
                min={0}
                style={{ width: '100%' }}
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
                extraText="录制与上传已经解耦；这里仅控制上传本身的并发。"
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
              <Text type="tertiary">
                旧平台插件和开发者字段已经从普通界面隐藏。尚未迁移的兼容字段仍保存在配置文件中，不会因为保存本页而被删除。
              </Text>
              <div style={{ marginTop: 16 }}>
                <Button theme="solid" onClick={() => formRef.current?.submitForm()}>保存设置</Button>
              </div>
            </Card>
          </Form>
        </div>
      </Content>
    </>
  )
}

export default ReplaySettings
