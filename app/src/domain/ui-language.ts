// 界面语言的译文状况：哪些语言已有译文，设置为某种语言时界面文案实际是什么语言。
// 外壳按它写 <html lang>，设置页按它决定语言下拉里给哪些语言，两处同一份。

import type { UiLanguage } from "../ipc/config";

/** 已有译文的界面语言。English 还没有译文；某种语言的译文落地后加进来，<html lang> 与设置页的语言下拉随之跟上。 */
const TRANSLATED: ReadonlySet<UiLanguage> = new Set<UiLanguage>(["zh-CN"]);

/** 没有译文时界面文案落回的语言，与 index.html 自带的 lang 一致。 */
const SOURCE_TEXT_LANG: UiLanguage = "zh-CN";

/** 设置为 language 时界面文案实际所用的语言。 */
export function textLanguage(language: UiLanguage): UiLanguage {
  return TRANSLATED.has(language) ? language : SOURCE_TEXT_LANG;
}
