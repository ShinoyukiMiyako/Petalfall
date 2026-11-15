// 使用须知的确认状态：首次启动、以及须知改版之后的第一次启动，要先读完须知并同意，才进首次启动向导或主界面。
// 版本号只写在随产物分发的须知文件文首（见 lib/legal-notice），本机只记下确认过的那一版与时刻（后端配置的 usage_notice）；
// 两边一比就知道这次要不要展示。旧配置没有这一项、这一项读不懂，后端都交回 null，按没确认过处理。

import { usageNoticeAcceptance } from "../ipc/system";
import { loadUsageNotice } from "../lib/app-info";
import type { LegalNotice } from "../lib/legal-notice";

/** 启动时读到的须知与本机确认过的版本。 */
export interface UsageNoticeState {
  notice: LegalNotice;
  /** 本机确认过的须知版本；从没确认过为 null。 */
  acceptedVersion: number | null;
}

/** 启动时读一次，须知文件与确认记录并行读。须知读不出来、文首不合约定时照实报错，不当成这次不用展示。 */
export async function loadUsageNoticeState(): Promise<UsageNoticeState> {
  const [notice, acceptance] = await Promise.all([loadUsageNotice(), usageNoticeAcceptance()]);
  return { notice, acceptedVersion: acceptance === null ? null : acceptance.version };
}

/**
 * 这次要不要先展示须知：确认过的不是文件里的这一版就要，从没确认过也算。只比相等、不比大小：
 * 本机只记最后确认的一版，装回旧版本时文件换回了旧的一版，与记下的不是同一份条款，同样要重新确认。
 */
export function noticePending(state: UsageNoticeState): boolean {
  return state.acceptedVersion !== state.notice.version;
}
