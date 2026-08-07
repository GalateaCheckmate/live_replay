import useSWR from "swr";

import {
  BiliType,
  fetcher,
  LiveStreamerEntity, proxy,
  User
} from "./api-streamer";
import {useEffect, useState} from "react";


export default function useStreamers() {
  const { data, error, isLoading } = useSWR<LiveStreamerEntity[]>("/v1/streamers", fetcher);

  return {
    isLoading,
    streamers: data,
  };
}

export function useBiliUsers() {
  const {data, error, isLoading} = useSWR<User[]>("/v1/users", fetcher);
  const [list, setList] = useState<any[]>([]);
  useEffect(() => {
    if (!data || data.length === 0) {
      setList([])
      return;
    }
    const updateList = async (item: User) => {
      try {
        const res = await fetcher(`/bili/space/myinfo?user=${item.value}`, undefined);
        const pRes = await proxy(`/bili/proxy?url=${res.data?.face}`);
        const myBlob = await pRes.blob();

        return {
          ...item,
          name: res.data.name,
          face: URL.createObjectURL(myBlob),
        };
      } catch (error) {
        console.error(error);
        const pRes = await proxy("/bili/proxy?url=https://i0.hdslb.com/bfs/face/member/noface.jpg");
        const myBlob = await pRes.blob();
        return {
          ...item,
          name: "Cookie已失效",
          face: URL.createObjectURL(myBlob),
        };
      }
    };
    Promise.all(data.map(updateList)).then(setList);
  }, [data])

  return {
    isLoading,
    isError: error,
    biliUsers: list,
  };
}

export function useTypeTree(userCookie?: string) {
  const key = userCookie
    ? `/bili/archive/pre?user=${encodeURIComponent(userCookie)}`
    : '/bili/archive/pre';
  const { data: archivePre, error, isLoading } = useSWR(key, fetcher);

  const mapType = (type: BiliType): any => ({
    label: type.name,
    value: type.id,
    name: type.name,
    id: type.id,
    children: (type.children ?? []).map(mapType),
  });
  const types = archivePre?.data?.typelist;
  const treeData = Array.isArray(types) ? types.map(mapType) : [];

  return {
    isLoading,
    isError: error,
    typeTree: treeData,
  };
}
