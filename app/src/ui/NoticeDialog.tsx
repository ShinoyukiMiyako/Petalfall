// 使用须知与隐私说明的弹窗：关于页的支持卡打开这两份，使用须知页在同意之前打开隐私说明，几处是同一个弹窗。
// 文件随产物分发，由调用方按需去读（弹窗打开时才读，整个会话复用），读到的结果与这一份的说法一起交进来。

import { useMemo } from "react";
import type { LegalNotice } from "../lib/legal-notice";
import { LoadedBody, type LoadedResource } from "./LoadedBody";
import { Modal } from "./Modal";
import s from "./NoticeDialog.module.css";
import { RichText } from "./RichText";
import { parseMarkdown } from "./richtext-parse";

/** 一份须知的说法：标题（也是正文区的名字）、读取中、读失败与重试，以及副标题怎样写文首的版本号。 */
export interface NoticeDialogCopy {
  title: string;
  loading: string;
  failed: string;
  retry: string;
  version: (version: number) => string;
}

/** 须知正文按受限富文本画：加粗的重要条款要看得出加粗。原文只用标题、段落、平铺列表、粗体、行内代码与链接，受限富文本画得全。 */
function NoticeBody({ body, label }: { body: string; label: string }) {
  const blocks = useMemo(() => parseMarkdown(body), [body]);
  return (
    <div role="region" aria-label={label} tabIndex={0}>
      <RichText blocks={blocks} />
    </div>
  );
}

/**
 * 内容一定比窗口长，打开就取满高。版本号读到之前先占住副标题这一行，读到后原位淡入；读失败（含文首不合约定）就没有这一行，正文位写明原因。
 * 正文里的链接经外部链接出口打开，出口由打开弹窗的那一层注入：失败写在哪里（通知或页面上）由它决定。
 */
export function NoticeDialog({ open, onClose, notice, copy }: { open: boolean; onClose: () => void; notice: LoadedResource<LegalNotice>; copy: NoticeDialogCopy }) {
  const subtitle = notice.status === "ready" ? copy.version(notice.data.version) : notice.status === "loading" ? null : undefined;
  return (
    <Modal open={open} onClose={onClose} size="md" icon="file-text" title={copy.title} subtitle={subtitle} className={s.noticeDialog}>
      <LoadedBody resource={notice} loading={copy.loading} failed={copy.failed} retry={copy.retry}>
        {(data) => <NoticeBody body={data.body} label={copy.title} />}
      </LoadedBody>
    </Modal>
  );
}
