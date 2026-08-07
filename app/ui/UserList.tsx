import React, { useCallback, useRef, useState } from 'react'
import { fetcher, requestDelete, sendRequest } from '../lib/api-streamer'
import { useSWRConfig } from 'swr'
import {
  Button,
  Form,
  List,
  Modal,
  Notification,
  Radio,
  RadioGroup,
  Row,
  SideSheet,
  Toast,
  Typography,
} from '@douyinfe/semi-ui'
import AvatarCard from './AvatarCard'
import { IconPlusCircle } from '@douyinfe/semi-icons'
import { FormApi } from '@douyinfe/semi-ui/lib/es/form'
import useSWRMutation from 'swr/mutation'
import { useBiliUsers } from '../lib/use-streamers'
import QRcode from '@/app/ui/QRcode'
import { useWindowSize } from 'react-use'

type UserListProps = {
  onCancel?: (e?: React.MouseEvent<Element, MouseEvent> | React.KeyboardEvent<Element>) => void
  visible?: boolean
}

const UserList: React.FC<UserListProps> = ({ onCancel, visible }) => {
  const { trigger } = useSWRMutation('/v1/users', sendRequest)
  const { trigger: deleteUser } = useSWRMutation('/v1/users', requestDelete)
  const { mutate } = useSWRConfig()
  const { biliUsers: list } = useBiliUsers()
  const [modalVisible, setVisible] = useState(false)
  const [confirmLoading, setConfirmLoading] = useState(false)
  const { width } = useWindowSize()
  const api = useRef<FormApi>()
  const [value, setValue] = useState<number>()
  const [panel, setPanel] = useState<React.ReactNode>(null)

  const showDialog = () => {
    setValue(undefined)
    setPanel(null)
    setVisible(true)
  }

  const addUser = useCallback(async (cookiePath: string) => {
    if (!cookiePath) return
    setConfirmLoading(true)
    try {
      const encodedCookie = encodeURIComponent(cookiePath)
      const ret = await fetcher(`/bili/space/myinfo?user=${encodedCookie}`, undefined)
      if (ret.code) {
        throw new Error(ret.message)
      }
      await trigger({
        key: 'bilibili-cookies',
        value: cookiePath,
      })
      await mutate('/v1/users')
      setVisible(false)
      setValue(undefined)
      setPanel(null)
      Toast.success('B站投稿账号已添加')
    } catch (e: any) {
      let messageObj = e.message
      try {
        messageObj = JSON.parse(messageObj).error
      } catch {
      }
      Notification.error({
        title: '添加账号失败',
        content: <Typography.Paragraph style={{ maxWidth: 450 }}>{messageObj}</Typography.Paragraph>,
        duration: 0,
      })
    } finally {
      setConfirmLoading(false)
    }
  }, [mutate, trigger])

  const handleOk = async () => {
    const values = await api.current?.validate()
    if (values?.value) {
      await addUser(values.value)
    }
  }

  const handleCancel = () => {
    setVisible(false)
    setValue(undefined)
    setPanel(null)
  }

  const removeUser = async (id: number) => {
    try {
      await deleteUser(id)
      await mutate('/v1/users')
      Toast.success('账号已删除')
    } catch (e: any) {
      Notification.error({
        title: '删除账号失败',
        content: <Typography.Paragraph style={{ maxWidth: 450 }}>{e.message}</Typography.Paragraph>,
        duration: 0,
      })
    }
  }

  const onChange = (e: any) => {
    const nextValue = Number(e.target.value)
    setValue(nextValue)
    if (nextValue === 2) {
      setPanel(<QRcode onSuccess={addUser} />)
      return
    }
    setPanel(
      <Form getFormApi={formApi => (api.current = formApi)}>
        <Form.Input
          field="value"
          label="Cookie 文件路径"
          trigger="blur"
          placeholder="data/123456.json"
          rules={[{ required: true, message: '请选择或填写 Cookie 文件路径' }]}
        />
      </Form>
    )
  }

  return (
    <SideSheet
      title={<Typography.Title heading={4}>B站投稿账号</Typography.Title>}
      visible={visible}
      width={Math.min(448, width || 448)}
      footer={
        <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
          <Button
            onClick={showDialog}
            icon={<IconPlusCircle size="large" />}
            theme="solid"
          >
            添加账号
          </Button>
        </div>
      }
      headerStyle={{ borderBottom: '1px solid var(--semi-color-border)' }}
      bodyStyle={{ borderBottom: '1px solid var(--semi-color-border)' }}
      onCancel={onCancel}
    >
      {(list ?? []).length === 0 ? (
        <Typography.Text type="tertiary">还没有投稿账号。点击下方“添加账号”，推荐使用扫码登录。</Typography.Text>
      ) : (
        <List
          dataSource={list}
          split={false}
          size="small"
          style={{ flexBasis: '100%', flexShrink: 0 }}
          renderItem={item => (
            <AvatarCard
              url={item.face}
              abbr={item.name}
              label={item.name}
              value={item.value}
              onRemove={async () => await removeUser(item.id)}
            />
          )}
        />
      )}

      <Modal
        title="添加 B站投稿账号"
        visible={modalVisible}
        onOk={handleOk}
        style={{ width: 'min(600px, 90vw)' }}
        onCancel={handleCancel}
        closeOnEsc={true}
        confirmLoading={confirmLoading}
        okButtonProps={{ disabled: value === 2 || value === undefined }}
        bodyStyle={{
          overflow: 'auto',
          maxHeight: 'calc(100vh - 320px)',
          paddingLeft: 10,
          paddingRight: 10,
        }}
      >
        <Typography.Paragraph type="tertiary">
          推荐扫码登录。登录成功后账号会自动加入 Live Replay，可在不同主播之间复用。
        </Typography.Paragraph>
        <Row type="flex" justify="center">
          <RadioGroup type="button" buttonSize="large" onChange={onChange} value={value}>
            <Radio value={2}>扫码登录</Radio>
            <Radio value={1}>Cookie 文件</Radio>
          </RadioGroup>
        </Row>
        <Row>{panel}</Row>
      </Modal>
    </SideSheet>
  )
}

export default UserList
