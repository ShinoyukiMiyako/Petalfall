import { motion } from "framer-motion";
import { Icon } from "../../art/Icon";
import { successMark } from "../../lib/motion";
import { Button } from "../../ui/Button";
import { Callout } from "../../ui/Callout";
import type { Channel, InstallMode } from "../contract";
import { modeTitle, type SetupCopy } from "../copy";
import { Frame } from "../Frame";
import styles from "./Done.module.css";

export interface DoneProps {
  copy: SetupCopy;
  mode: InstallMode;
  /** 这次换到的通道（model 的 switchingTo），标题据此说「换到」；为 null 时按模式说。 */
  switching: Channel | null;
  /** 换到更慢通道之前装着的通道（model 的 switchedFrom），只在 mode 为 switch 时有值，说明据此改用 copy.done.switchKept。 */
  switchedFrom: Channel | null;
  version: string;
  /** 不是换到更慢通道时：为真说明用 copy.done.kept，否则用 copy.done.firstRun。 */
  keptData: boolean;
  /** 提权运行：不提供打开，提示从开始菜单打开。 */
  elevated: boolean;
  /** 点了打开之后主按钮转加载。 */
  opening: boolean;
  onOpen(): void;
  onClose(): void;
}

/** 换到更慢通道装上的是更早的版本，原来那个通道里新加的设置项可能回到默认值，不能照别的安装说设置都还在。 */
function leadOf(copy: SetupCopy, mode: InstallMode, switchedFrom: Channel | null, switching: Channel | null, keptData: boolean): string {
  const text = copy.done;
  if (mode !== "switch") return keptData ? text.kept : text.firstRun;
  if (switchedFrom === null || switching === null) throw new Error(`换通道安装要两边的通道，收到的是 ${switchedFrom} 与 ${switching}`);
  return text.switchKept(copy.channel[switchedFrom], copy.channel[switching]);
}

/**
 * 视图自己不打开：由用户点击，引导程序才掌握退出时机；更新模式下 App 替用户走同一条打开的路，这里照样只显示加载。
 * 焦点落在下一步最该按的按钮上，Enter 直接生效。
 * 提权时不给打开：提权启动和普通启动用的是两个数据根。
 */
export function Done({ copy, mode, switching, switchedFrom, version, keptData, elevated, opening, onOpen, onClose }: DoneProps) {
  const text = copy.done;
  return (
    <Frame
      title={
        <span className={styles.title}>
          {/* 装好了的落点：徽记从小处弹出来。减少动画时根部 MotionConfig 关掉缩放，只剩淡入 */}
          <motion.span className={styles.mark} aria-hidden="true" {...successMark}>
            <Icon name="check" size={18} />
          </motion.span>
          {modeTitle(copy, text.title, mode, switching, version)}
        </span>
      }
      lead={leadOf(copy, mode, switchedFrom, switching, keptData)}
      footer={
        <>
          <Button autoFocus={elevated} onClick={onClose}>
            {text.close}
          </Button>
          {!elevated && (
            <Button variant="primary" autoFocus loading={opening} onClick={onOpen}>
              {text.open}
            </Button>
          )}
        </>
      }
    >
      {elevated && <Callout tone="info">{text.elevated}</Callout>}
    </Frame>
  );
}
