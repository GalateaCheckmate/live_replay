// Fetcher implementation. // The extra argument will be passed via the `arg` property of the 2nd parameter.// In the example below, `arg` will be `'my_token'`
export const API_BASE = process.env.NEXT_PUBLIC_API_SERVER ?? '';
export async function sendRequest<T>(url: string, { arg }: { arg: T }) {
	const res = await fetch(API_BASE + url, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(arg),
	});
	await handleResponse(res);
	return res.json();
}

export const fetcher = async (input: RequestInfo | URL, init?: RequestInit) => {
	const res = await fetch(API_BASE + input, init);
	await handleResponse(res);
	return res.json();
};

export const proxy = async (input: RequestInfo | URL, init?: RequestInit) => {
	const res = await fetch(API_BASE + input, init);
	await handleResponse(res);
	return res;
};

export async function requestDelete<T>(url: string, { arg }: { arg: T }) {
	const res = await fetch(`${API_BASE}${url}/${arg}`, { method: 'DELETE' });
	await handleResponse(res);
	return res;
}

export async function put<T>(url: string, { arg }: { arg: T }) {
	const res = await fetch(`${API_BASE}${url}`, {
		method: 'PUT',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(arg),
	});
	await handleResponse(res);
	return res;
}

async function handleResponse(res: Response) {
	if (res.status === 401) {
		const returnTo = encodeURIComponent(window.location.pathname + window.location.search);
		window.location.href = `/login?next=${returnTo}`;
		throw new Error('Unauthorized');
	}

	if (!res.ok) {
		const text = await res.text().catch(() => '');
		throw new Error(text || `HTTP ${res.status}`);
	}
	return res;
}

export type ReplayUserState = 'waiting' | 'recording' | 'uploading' | 'error';

/** Live Replay 自己的稳定主播状态。 */
export interface ReplayStreamerState {
	id: number;
	name: string;
	url: string;
	enabled: boolean;
	user_state: ReplayUserState;
	pending_upload_parts: number;
	active_session_id?: number;
	active_session_started_at?: string;
	recording_elapsed_seconds?: number;
	recording_bytes?: number;
}

/** Live Replay 自己的主播设置视图；前端不再理解 upload template / override。 */
export interface ReplayStreamerSettings {
	id: number;
	url: string;
	name: string;
	enabled: boolean;
	user_cookie: string;
	title: string;
	tid: number;
	tags: string[];
	copyright: number;
	copyright_source: string;
	description: string;
	is_only_self: number;
	segment_minutes: number;
	delete_after_success: boolean;
}

type Credit = {
	username: string;
	uid: number;
};

// Compatibility types below are retained while old backend endpoints are migrated.
export interface StudioEntity {
	id: number;
	template_name: string;
	user_cookie: string;
	copyright: number;
	copyright_source: string;
	tid: number;
	cover_path: string;
	title: string;
	description: string;
	dynamic: string;
	tags: string[];
	dtime: number;
	mission_id?: number;
	dolby: number;
	hires: number;
	no_reprint: number;
	is_only_self: number;
	up_selection_reply: number;
	up_close_reply: number;
	up_close_danmu: number;
	charging_pay: number;
	credits: Credit[];
	uploader: string;
	extra_fields?: string;
}

export interface LiveStreamerEntity {
	id: number;
	url: string;
	enabled: boolean;
	upload_streamers_id?: number;
	remark: string;
	filename: string;
	split_time?: number;
	split_size?: number;
	upload_id?: number;
	status?: string;
	upload_status?: string;
	recording_elapsed_seconds?: number;
	recording_bytes?: number;
	statusTag?: React.ReactNode;
	format?: string;
    time_range?: string | Date[];
    excluded_keywords?: string[];
	preprocessor?: Record<'run', string>[];
	segment_processor?: Record<'run', string>[];
	downloaded_processor?: Record<'run', string>[];
	postprocessor?: (Record<'run' | 'mv', string> | 'rm')[];
	opt_args?: string[];
	override?: Record<string, any>;
}

export interface BiliType {
	id: number;
	children: BiliType[];
	name: string;
	desc: string;
}

export interface User {
	id: number;
	name: string;
	value: string;
	platform: string;
}

export interface FileList {
	key: number;
	name: string;
	updateTime: number;
	size: number;
}
