// 随产物分发的文件（协议全文、附加条款、素材许可、须知、第三方许可清单）读不出来时的错误。
// message 是带称呼的整句：启动错误页与日志这类没有标题点名是哪份文件的地方，照原样就看得出是哪份没读到。
// 弹窗与引导程序协议页的读失败提示，标题已经写了「X读不出来」，正文只放 reason，同一句不说两遍。

export class BundledFileError extends Error {
  /** 不带称呼的原因，如「/LICENSE.txt 返回 404」。 */
  readonly reason: string;

  /** what 是对这份文件的称呼，与读失败提示的标题同一个说法。 */
  constructor(what: string, reason: string) {
    super(`${what}读不出来：${reason}`);
    this.name = "BundledFileError";
    this.reason = reason;
  }
}

/** 读失败提示里放在标题下的原文：随产物分发的文件只给原因；别的错误（fetch 本身失败、内容解析不了）照原文。 */
export function readFailureReason(error: Error): string {
  return error instanceof BundledFileError ? error.reason : error.message;
}
