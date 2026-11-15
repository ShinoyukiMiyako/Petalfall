// 使用须知的关口：首次启动与须知改版后的第一次启动，在首次启动向导与主界面之前整窗展示须知全文，同意才放行，不同意就退出。
//
// 挂在状态容器之内、外壳核心之外：须知展示期间状态容器照常读本机的几片数据（不联网），同意时首次启动与否已经知道，
// 向导或主界面一次就挂对；外壳核心（全局快捷键、文件拖放）、启动器更新检查、主页的资讯与服务器状态都在关口后面，
// 同意之前一样都不挂。须知文件与确认记录启动时与外观偏好一起读（见 main.tsx），读不出来进启动失败页，不在这里兜底。
//
// 同意之后整页淡出、面板轻微放大，向导或主界面已经挂在它底下，淡出即露出，它们按应用启动时的那一屏处理，不另播入场。
// 外壳的通知容器在关口后面还没挂上，这一页的失败（记不下同意、窗口操作、打开链接）写在底栏上方。

import { AnimatePresence, motion, useReducedMotionConfig } from "framer-motion";
import { useCallback, useId, useMemo, useState, type ReactNode } from "react";
import { Logo } from "../art/Logo";
import { ACTION_ERROR } from "../copy/flows";
import { SHELL_ACTION_ERROR, USAGE_NOTICE_COPY } from "../copy/shell";
import { acceptUsageNotice } from "../ipc/system";
import type { LegalNotice } from "../lib/legal-notice";
import { fade } from "../lib/motion";
import { useLauncherActions, useLauncherSelector } from "../state/launcher";
import { noticePending, type UsageNoticeState } from "../state/usage-notice";
import { Button } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { ExternalLinkProvider } from "../ui/external-link";
import { RichText } from "../ui/RichText";
import { parseMarkdown } from "../ui/richtext-parse";
import { WindowControls } from "./TitleBar";
import s from "./UsageNoticeGate.module.css";
import { runGuarded, type Runner } from "./useRunner";
import { useRevealWindow, useWindowFrame, useWindowPort } from "./window";

/** 退场走 fade.out，缩放也跟着它在同一段时间里走完：底下的界面要等它让开，不等弹簧静止。 */
const SCREEN_EXIT = { opacity: 0, transition: fade.out } as const;
const PANEL_EXIT = { scale: 1.02, transition: fade.out } as const;

export interface UsageNoticeGateProps {
  /** 启动时读到的须知与本机确认过的版本。 */
  state: UsageNoticeState;
  /** 同意之后（或这一版早已确认过）才挂上的外壳。 */
  children: ReactNode;
}

export function UsageNoticeGate({ state, children }: UsageNoticeGateProps) {
  const [shown] = useState(() => noticePending(state));
  const [agreed, setAgreed] = useState(!shown);
  // 展示过须知的这一次，窗口早已显示：首次启动与否还没读到就挂上外壳，会先画一帧主界面再换成向导
  const firstRunKnown = useLauncherSelector((s) => s.status.firstRun.kind !== "loading");
  const open = !agreed || (shown && !firstRunKnown);
  return (
    <>
      {!open && children}
      <AnimatePresence>
        {open && <NoticeScreen key="usage-notice" notice={state.notice} updated={state.acceptedVersion !== null} onAgreed={() => setAgreed(true)} />}
      </AnimatePresence>
    </>
  );
}

type Busy = "agree" | "refuse" | null;

interface Failure {
  title: string;
  body: string;
}

function NoticeScreen({ notice, updated, onAgreed }: { notice: LegalNotice; updated: boolean; onAgreed: () => void }) {
  useRevealWindow(true);
  const port = useWindowPort();
  const actions = useLauncherActions();
  const reduced = useReducedMotionConfig() === true;
  const titleId = useId();
  const [busy, setBusy] = useState<Busy>(null);
  const [failure, setFailure] = useState<Failure | null>(null);
  const run = useCallback<Runner>((work, title) => runGuarded(work, title, { error: (t, body) => setFailure({ title: t, body }) }), []);
  const reportFrame = useCallback((error: unknown) => void run(() => Promise.reject(error), SHELL_ACTION_ERROR.windowState), [run]);
  const frame = useWindowFrame(reportFrame);
  const openLink = useCallback((url: string) => void run(() => actions.openExternal(url), SHELL_ACTION_ERROR.openLink), [actions, run]);
  const blocks = useMemo(() => parseMarkdown(notice.body), [notice.body]);

  const agree = async () => {
    setBusy("agree");
    setFailure(null);
    const result = await run(() => acceptUsageNotice(notice.version), USAGE_NOTICE_COPY.agreeFailed);
    // 记下了就交给关口撤掉这一页，按钮停在进行中直到退场：不然退场前一帧它又变回可点
    if (result.ok) onAgreed();
    else setBusy(null);
  };

  // 与关闭确认之后的退出同一套收尾：先把任务记录写一次，再销毁窗口（唯一的窗口没了，进程随之退出）。什么都不记，下次启动照样先看到须知
  const refuse = async () => {
    setBusy("refuse");
    setFailure(null);
    await run(() => actions.flushTaskJournal(), ACTION_ERROR.saveJournal);
    await run(() => port.destroy(), SHELL_ACTION_ERROR.close);
    // 浏览器预览没有原生窗口，销毁什么都不做，留在这一页
    setBusy(null);
  };

  return (
    <motion.div className={s.root} data-sk-usage-notice="" exit={SCREEN_EXIT}>
      <div className={s.ambience} aria-hidden="true" />
      <header className={s.strip} data-tauri-drag-region="deep" data-blurred={!frame.focused || undefined}>
        <Logo size={20} className={s.logo} />
        <WindowControls maximized={frame.maximized} run={run} />
      </header>
      <div className={s.stage}>
        <motion.section className={s.panel} aria-labelledby={titleId} exit={reduced ? undefined : PANEL_EXIT}>
          <div className={s.head}>
            <p className={s.version}>{USAGE_NOTICE_COPY.version(notice.version)}</p>
            <h1 id={titleId} className={s.title}>
              {USAGE_NOTICE_COPY.title}
            </h1>
            <p className={s.lead}>{updated ? USAGE_NOTICE_COPY.leadUpdated : USAGE_NOTICE_COPY.lead}</p>
          </div>
          <ExternalLinkProvider open={openLink}>
            <div className={s.scroll} role="region" aria-label={USAGE_NOTICE_COPY.title} tabIndex={0}>
              <RichText blocks={blocks} />
            </div>
          </ExternalLinkProvider>
          {failure !== null && (
            <Callout tone="danger" announce fullText className={s.failure}>
              <span className={s.failureTitle}>{failure.title}</span>
              {failure.body}
            </Callout>
          )}
          <footer className={s.footer}>
            {/* 拒绝与同意同样醒目：不同意是正经的选项，不做成不起眼的文字按钮 */}
            <Button variant="secondary" loading={busy === "refuse"} disabled={busy === "agree"} focusableWhenDisabled onClick={() => void refuse()}>
              {USAGE_NOTICE_COPY.refuse}
            </Button>
            <Button variant="primary" loading={busy === "agree"} disabled={busy === "refuse"} focusableWhenDisabled onClick={() => void agree()}>
              {USAGE_NOTICE_COPY.agree}
            </Button>
          </footer>
        </motion.section>
      </div>
    </motion.div>
  );
}
