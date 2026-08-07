'use client'
import styles from './page.module.css'
import { useCallback, useMemo, useState, useEffect } from 'react'
import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { Button, Nav } from '@douyinfe/semi-ui'
import { OnSelectedData } from '@douyinfe/semi-ui/lib/es/navigation'
import { Layout as SeLayout } from '@douyinfe/semi-ui/lib/es/layout'
import {
    IconDoubleChevronLeft,
    IconDoubleChevronRight,
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
    const [selectedKeys, setSelectedKeys] = useState<any>([routeKey(pathname)])
    const { width } = useWindowSize()
    const [isCollapsed, setIsCollapsed] = useState(width <= 640)
    const [mode, setMode] = useState(
        (typeof window !== 'undefined' && localStorage.getItem('mode')) || 'auto'
    )
    const systemTheme = useSystemTheme()
    useTheme(mode, systemTheme)
    const navStyle = isCollapsed ? { height: '100%', overflow: 'visible' } : { height: '100%' }

    useEffect(() => {
        setSelectedKeys([routeKey(pathname)])
    }, [pathname])

    useEffect(() => {
        if (width <= 640) setIsCollapsed(true)
    }, [width])

    // 主导航只保留 Live Replay 自己的概念。
    // 旧 biliup 任务平台、投稿模板、Job、实时内部状态页继续保留路由用于兼容/排障，
    // 但不再作为普通用户工作流的一部分暴露在侧栏里。
    const items = useMemo(
        () => [
            {
                itemKey: 'home',
                text: '主播',
                icon: navIcon('#ffaa00ff', <IconHome size="small" />),
            },
            {
                itemKey: 'replay',
                text: '场次与投稿',
                icon: navIcon('rgb(250 102 76)', <IconVideoListStroked size="small" />),
            },
            {
                itemKey: 'history',
                text: '录制历史',
                icon: navIcon('#5ac262ff', <IconHistory size="small" />),
            },
            {
                itemKey: 'settings',
                text: '设置',
                icon: navIcon('#6b6c75ff', <IconSetting size="small" />),
            },
        ],
        []
    )

    const renderWrapper = useCallback(({ itemElement, props }: any) => {
        const routerMap: Record<string, string> = {
            home: '/',
            replay: '/replay',
            history: '/history',
            settings: '/dashboard',
        }
        if (!routerMap[props.itemKey]) return itemElement
        return (
            <Link style={{ textDecoration: 'none', fontWeight: '600 !important' }} href={routerMap[props.itemKey]}>
                {itemElement}
            </Link>
        )
    }, [])

    const onSelect = (data: OnSelectedData) => setSelectedKeys([...data.selectedKeys])
    const onCollapseChange = useCallback(() => setIsCollapsed(!isCollapsed), [isCollapsed])

    return (
        <html lang="zh-Hans">
        <body style={{ width: '100%' }}>
        <SeLayout className="components-layout-demo semi-light-scrollbar">
            <Sider>
                <Nav
                    style={navStyle}
                    selectedKeys={selectedKeys}
                    isCollapsed={isCollapsed}
                    renderWrapper={renderWrapper}
                    items={items}
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

function routeKey(pathname: string) {
    if (pathname.startsWith('/replay')) return 'replay'
    if (pathname.startsWith('/history')) return 'history'
    if (pathname.startsWith('/dashboard')) return 'settings'
    return 'home'
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
