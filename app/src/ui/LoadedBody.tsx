// 弹窗里按需读取的内容：读失败写明原因、给重试，不静默留白；读到了交给 children 渲染。
// 关于页的协议、许可、须知弹窗与使用须知页的隐私说明弹窗共用，读取中、读失败在各处是同一种样子。

import type { ReactNode } from "react";
import { Spinner } from "../art/Spinner";
import { readFailureReason } from "../lib/bundled-file";
import { useLoadingGate } from "../lib/loading-gate";
import { Button } from "./Button";
import { Callout } from "./Callout";
import { Loadable } from "./Loadable";
import s from "./LoadedBody.module.css";

/** 按需读到的一份数据，与状态层的 ResourceResult 同形：组件库不依赖状态层，这里只写用得到的几项。 */
export type LoadedResource<T> = { status: "loading" } | { status: "ready"; data: T } | { status: "error"; error: Error; reload(): void };

export interface LoadedBodyProps<T> {
  resource: LoadedResource<T>;
  /** 转圈旁的那句，也是读屏播报的读取中说明。 */
  loading: string;
  /** 读失败时 Callout 的标题，点名是哪份读不出来；正文是失败原因（readFailureReason）。 */
  failed: string;
  /** 读失败时重读按钮的文案。 */
  retry: string;
  children: (data: T) => ReactNode;
}

/** 读取中的转圈经加载门：随产物分发的文件通常几十毫秒就读到，那时转圈只占位不显形，内容直接淡入。 */
export function LoadedBody<T>({ resource, loading, failed, retry, children }: LoadedBodyProps<T>) {
  const gate = useLoadingGate(resource.status === "loading");
  if (resource.status === "error") {
    return (
      <Callout
        tone="danger"
        announce
        fullText
        action={
          <Button size="sm" variant="ghost" onClick={() => resource.reload()}>
            {retry}
          </Button>
        }
      >
        <span className={s.calloutTitle}>{failed}</span>
        <span className={s.errorText}>{readFailureReason(resource.error)}</span>
      </Callout>
    );
  }
  return (
    // 骨架层对读屏隐藏，转圈旁的那句读屏听不到：同一句交给加载层的播报区，转圈显形时才念
    <Loadable
      view={gate.view}
      busyLabel={loading}
      skeleton={
        <div className={s.loading}>
          <Spinner size={20} />
          <span>{loading}</span>
        </div>
      }
    >
      {() => {
        if (resource.status !== "ready") throw new Error("加载门放行内容时文件必须已经读到");
        return children(resource.data);
      }}
    </Loadable>
  );
}
