// 下载：CurseForge 上作者关掉了第三方分发的文件，要不要按文件编号拼出地址直接下。默认关，遇到时列成清单让用户手动下载；
// 打开就绕开了作者的设置，所以只摆一行开关与说明，不替用户做主。进入本步不需要加载任何东西。

import { useRef, useState } from "react";
import { useRunner } from "../../shell/useRunner";
import { useLauncherActions, useLauncherSelector } from "../../state/launcher";
import { SettingRow } from "../../ui/SettingRow";
import { Toggle } from "../../ui/Toggle";
import { DOWNLOAD_STEP_COPY } from "./copy";
import s from "./onboarding.module.css";

/** 开关即时写入；写入中先显示新值，结束后回到状态层的值（失败即回到原值）。 */
export function StepDownload() {
  const settings = useLauncherSelector((s) => s.settings);
  const actions = useLauncherActions();
  const run = useRunner();
  const [draft, setDraft] = useState<boolean | null>(null);
  const seq = useRef(0);
  if (settings === null) throw new Error("向导要在配置读到之后渲染");
  const checked = draft === null ? settings.downloadRestrictedCurseforgeFiles : draft;
  const change = (next: boolean) => {
    const id = ++seq.current;
    setDraft(next);
    void run(() => actions.updateSettings({ downloadRestrictedCurseforgeFiles: next }), DOWNLOAD_STEP_COPY.saveFailed).then(() => {
      // 连续切换时只认最后一次
      if (seq.current === id) setDraft(null);
    });
  };
  return (
    <div className={s.stepBody}>
      <SettingRow title={DOWNLOAD_STEP_COPY.restricted} description={DOWNLOAD_STEP_COPY.restrictedDesc}>
        <Toggle checked={checked} onChange={change} ariaLabel={DOWNLOAD_STEP_COPY.restricted} />
      </SettingRow>
    </div>
  );
}
