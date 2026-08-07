'use client'
import { Layout, Modal, Nav, Typography } from '@douyinfe/semi-ui'
import { IconUserCardVideo, IconVideoListStroked } from '@douyinfe/semi-icons'
import { Table } from '@douyinfe/semi-ui'
import { SortOrder } from '@douyinfe/semi-ui/lib/es/table'
import useSWR from 'swr'
import { API_BASE, fetcher, FileList } from '@/app/lib/api-streamer'
import { useState } from 'react'
import dynamic from 'next/dynamic'
import { humDate } from '@/app/lib/utils'

const Players = dynamic(() => import('@/app/ui/Player'), {
  ssr: false,
})

const encodeRecordingPath = (path: string) => path
  .split('/')
  .filter(Boolean)
  .map(part => encodeURIComponent(part))
  .join('/')

export default function Home() {
  const { Header, Content } = Layout
  const { data } = useSWR<FileList[]>('/v1/videos', fetcher, { refreshInterval: 5000 })
  const { Text } = Typography
  const [filePath, setFilePath] = useState<string>()
  const columns = [
    {
      title: '标题',
      dataIndex: 'name',
      render: (text: string) => <Text strong>{text}</Text>,
    },
    {
      title: '大小',
      dataIndex: 'size',
      render: (size: number) => `${(size / 1024 / 1024).toFixed(2)} MB`,
    },
    {
      title: '更新日期',
      dataIndex: 'updateTime',
      defaultSortOrder: 'descend' as SortOrder,
      sorter: (a: FileList, b: FileList) => a.updateTime - b.updateTime,
      render: (time: number) => humDate(time),
    },
    {
      title: '',
      dataIndex: 'operate',
      render: (_: unknown, record: FileList) => (
        <IconUserCardVideo
          style={{ cursor: 'pointer' }}
          onClick={() => showDialog(record.path || record.name)}
        />
      ),
    },
  ]
  const [visible, setVisible] = useState(false)
  const showDialog = (path: string) => {
    setVisible(true)
    setFilePath(path)
  }
  const handleCancel = () => {
    setVisible(false)
    setFilePath(undefined)
  }
  const playbackUrl = filePath ? `${API_BASE}/static/${encodeRecordingPath(filePath)}` : ''

  return (
    <>
      <Header style={{ backgroundColor: 'var(--semi-color-bg-1)' }}>
        <Nav
          style={{ border: 'none' }}
          header={
            <>
              <div
                style={{
                  backgroundColor: 'rgba(var(--semi-green-4), 1)',
                  borderRadius: 'var(--semi-border-radius-large)',
                  color: 'var(--semi-color-bg-0)',
                  display: 'flex',
                  padding: '6px',
                }}
              >
                <IconVideoListStroked size="large" />
              </div>
              <h4 style={{ marginLeft: '12px' }}>本地录像</h4>
            </>
          }
          mode="horizontal"
        />
      </Header>
      <Content
        style={{
          paddingLeft: 12,
          paddingRight: 12,
          backgroundColor: 'var(--semi-color-bg-0)',
        }}
      >
        <main>
          <Table size="small" columns={columns} dataSource={data ?? []} rowKey="key" />
        </main>
        <Modal
          visible={visible}
          onCancel={handleCancel}
          closeOnEsc={true}
          style={{ width: 'min(600px, 90vw)' }}
          size="large"
          bodyStyle={{ height: 500 }}
          footer={null}
        >
          {playbackUrl && <Players url={playbackUrl} />}
        </Modal>
      </Content>
    </>
  )
}
