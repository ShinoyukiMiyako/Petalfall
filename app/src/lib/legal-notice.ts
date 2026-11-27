// 使用须知与隐私说明的文首约定：第一行是「# 标题」，下一个非空行是「版本：N」（N 从 1 起），其后是正文。
// 版本号只写在文件里、不在代码里另记一份：改了条款就在同一处把号加一，两处分开记迟早对不上。
// 首次启动要按这个号判断用户确认过的是不是当前这一版，所以文首不合约定时直接报错，不猜一个号。

import { BundledFileError } from "./bundled-file";

/** 一份须知读出来的样子。 */
export interface LegalNotice {
  /** 文首「版本：N」的 N。 */
  version: number;
  /** 标题与版本行之后的正文（Markdown），行尾统一成 \n，首尾不留空行。 */
  body: string;
}

const HEAD = /^# \S[^\n]*\n+版本：([1-9][0-9]*)\n/;

/**
 * 按文首约定拆出版本号与正文；what 是报错里对这份文件的称呼。文首不合约定也算这份文件读不出来，报 BundledFileError：
 * 读失败提示的标题已点名是哪份，正文只放原因。
 */
export function parseLegalNotice(text: string, what: string): LegalNotice {
  const normalized = text.replace(/\r\n?/g, "\n");
  const head = HEAD.exec(normalized);
  if (head === null) throw new BundledFileError(what, "文首应是一行「# 标题」，接着一行「版本：N」（N 是从 1 起的整数）");
  return { version: Number(head[1]), body: normalized.slice(head[0].length).trim() };
}
