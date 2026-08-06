'use client'
import styles from './page.module.css'
import { useCallback, useMemo, useState, useEffect } from 'react'
import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { Button, Nav } from '@douyinfe/semi-ui'
import { OnSelectedData } from '@douyinfe/semi-ui/lib/es/navigation'
import { Layout as SeLayout } from '@douyinfe/semi-ui/lib/es/layout'
import {
    IconCloudStroked,
    IconCustomerSupport,
    IconDoubleChevronLeft,
    IconDoubleChevronRight,
    IconStar,
    IconVideoListStroked,
    IconHome,
    IconSetting,
    IconHistory,
} from '@douyinfe/semi-icons'
import Image from 'next/image'
import ThemeButton from '../ui/ThemeButton'
import { useSystemTheme, useTheme } from '../lib/utils'
import { useWindowSize } from 'react-use'

export default function Layout({ children }: { children: React.ReactNode }) {
    const { Sider } = SeLayout
    const pathname = usePathname()
    let initOpenKeys: any = []
    if (pathname.slice(1) === 'streamers' || pathname.slice(1) === 'history') {
        initOpenKeys = ['manager']
    }

    const [openKeys, setOpenKeys] = useState(initOpenKeys)
    const [selectedKeys, setSelectedKeys] = useState<any>([pathname.slice(1)])

    const { width } = useWindowSize()
    const [isCollapsed, setIsCollapsed] = useState(width <= 640)
    const [mode, setMode] = useState(
        (typeof window !== 'undefined' && localStorage.getItem('mode')) || 'auto'
    )
    const systemTheme = useSystemTheme()
    useTheme(mode, systemTheme)
    const navStyle = isCollapsed ? { height: '100%', overflow: 'visible' } : { height: '100%' }

    useEffect(() => {
        if (width <= 640) setIsCollapsed(true)
    }, [width])

    const items = useMemo(
        () =>
            [
                {
                    itemKey: 'home',
                    text: '主页',
                    icon: navIcon('#ffaa00ff', <IconHome size="small" />),
                },
                {
                    itemKey: 'manager',
                    text: '录播管理',
                    items: [
                        { itemKey: 'streamers', text: '直播管理' },
                        { itemKey: 'history', text: '历史记录' },
                    ],
                    icon: navIcon('#5ac262ff', <IconVideoListStroked size="small" />),
                },
                {
                    itemKey: 'replay',
                    text: '自动上传队列',
                    icon: navIcon('rgb(250 102 76)', <IconHistory size="small" />),
                },
                {
                    itemKey: 'upload-manager',
                    text: '投稿模板',
                    icon: navIcon('#885bd2ff', <IconCloudStroked size="small" />),
                },
                {
                    itemKey: 'dashboard',
                    text: '空间配置',
                    icon: navIcon('#6b6c75ff', <IconStar size="small" />),
                },
                {
                    itemKey: 'job',
                    text: '直播历史',
                    icon: navIcon('#ef7859', <IconHistory size="small" />),
                },
                {
                    text: '实时日志',
                    itemKey: 'logViewer',
                    icon: navIcon('rgba(var(--semi-blue-4), 1)', <IconCustomerSupport size="small" />),
                },
                {
                    text: '任务平台',
                    itemKey: 'status',
                    icon: navIcon('rgba(var(--semi-lime-2), 1)', <IconSetting size="small" />),
                },
            ].map((value: any) => {
                value.text = (
                    <div
                        style={{
                            color:
                                selectedKeys.some((key: string) => value.itemKey === key) ||
                                (selectedKeys.some((key: string) =>
                                        openKeys.some((o: string | number) => isSub(key, o))
                                    ) && openKeys.some((key: any) => value.itemKey === key))
                                    ? 'var(--semi-color-text-0)'
                                    : 'var(--semi-color-text-2)',
                            fontWeight: 600,
                        }}
                    >
                        {value.text}
                    </div>
                )
                return value
            }),
        [openKeys, selectedKeys]
    )

    const renderWrapper = useCallback(({ itemElement, props }: any) => {
        const routerMap: Record<string, string> = {
            home: '/',
            history: '/history',
            dashboard: '/dashboard',
            streamers: '/streamers',
            replay: '/replay',
            'upload-manager': '/upload-manager',
            job: '/job',
            status: '/status',
            logViewer: '/logviewer',
        }
        if (!routerMap[props.itemKey]) return itemElement
        return (
            <Link style={{ textDecoration: 'none', fontWeight: '600 !important' }} href={routerMap[props.itemKey]}>
                {itemElement}
            </Link>
        )
    }, [])

    const onSelect = (data: OnSelectedData) => setSelectedKeys([...data.selectedKeys])
    const onOpenChange = (data: any) => setOpenKeys([...data.openKeys])
    const onCollapseChange = useCallback(() => setIsCollapsed(!isCollapsed), [isCollapsed])

    return (
        <html lang="zh-Hans">
        <body style={{ width: '100%' }}>
        <SeLayout className="components-layout-demo semi-light-scrollbar">
            <Sider>
                <Nav
                    style={navStyle}
                    openKeys={openKeys}
                    selectedKeys={selectedKeys}
                    isCollapsed={isCollapsed}
                    renderWrapper={renderWrapper}
                    items={items}
                    onOpenChange={onOpenChange}
                    onSelect={onSelect}
                >
                    <Nav.Header
                        logo={<Image src="/logo.png" alt="Live Replay" height={10} width={20} />}
                        style={isCollapsed
                            ? { flexDirection: 'column', paddingLeft: 0, paddingRight: 0, paddingBottom: 0, gap: '8px' }
                            : { justifyContent: 'flex-start' }}
                        text="LIVE REPLAY"
                    >
                        <div
                            style={{
                                flexGrow: 1,
                                display: width <= 640 ? 'none' : 'flex',
                                flexDirection: 'row-reverse',
                                zIndex: 2,
                            }}
                        >
                            <Button
                                onClick={onCollapseChange}
                                type="tertiary"
                                className={styles.shadow}
                                theme="borderless"
                                icon={isCollapsed ? <IconDoubleChevronRight /> : <IconDoubleChevronLeft />}
                            />
                        </div>
                    </Nav.Header>
                    <Nav.Footer collapseButton={false}>
                        <ThemeButton mode={mode} setMode={setMode} systemTheme={systemTheme} />
                    </Nav.Footer>
                </Nav>
            </Sider>
            <SeLayout style={{ height: '100vh' }}>{children}</SeLayout>
        </SeLayout>
        </body>
        </html>
    )
}

function navIcon(backgroundColor: string, icon: React.ReactNode) {
    return (
        <div
            style={{
                backgroundColor,
                borderRadius: 'var(--semi-border-radius-medium)',
                color: 'var(--semi-color-bg-0)',
                display: 'flex',
                padding: '4px',
            }}
        >
            {icon}
        </div>
    )
}

function isSub(key1: string, key2: string | number) {
    const routerMap: any = { manager: ['streamers', 'history'] }
    return routerMap[key2]?.includes(key1)
}
