// 关于页用到的构建产物：第三方许可清单、协议全文、附加条款、素材许可、使用须知与隐私说明。

import { loadAdditionalTerms, loadAssetLicense, loadLicenseText, loadPrivacyNotice, loadThirdPartyLicenses, loadUsageNotice, type ThirdPartyManifest } from "../../lib/app-info";
import type { LegalNotice } from "../../lib/legal-notice";
import { useResource, type ResourceResult } from "../resource";

export type { ThirdPartyManifest, ThirdPartyPackage } from "../../lib/app-info";

/** third-party-licenses —— 清单体积大，只在许可弹窗打开时（enabled）才去读；整个会话读一次。 */
export function useThirdPartyLicenses(enabled: boolean): ResourceResult<ThirdPartyManifest> {
  return useResource(enabled ? "third-party-licenses" : null, loadThirdPartyLicenses);
}

/** license-text —— 协议全文，只在协议弹窗打开时（enabled）才去读；整个会话读一次。 */
export function useLicenseText(enabled: boolean): ResourceResult<string> {
  return useResource(enabled ? "license-text" : null, loadLicenseText);
}

/** additional-terms —— 附加条款，只在它的弹窗打开时（enabled）才去读；整个会话读一次。 */
export function useAdditionalTerms(enabled: boolean): ResourceResult<string> {
  return useResource(enabled ? "additional-terms" : null, loadAdditionalTerms);
}

/** asset-license —— 素材许可，只在它的弹窗打开时（enabled）才去读；整个会话读一次。 */
export function useAssetLicense(enabled: boolean): ResourceResult<string> {
  return useResource(enabled ? "asset-license" : null, loadAssetLicense);
}

/** usage-notice —— 使用须知，只在它的弹窗打开时（enabled）才去读；整个会话读一次。 */
export function useUsageNotice(enabled: boolean): ResourceResult<LegalNotice> {
  return useResource(enabled ? "usage-notice" : null, loadUsageNotice);
}

/** privacy-notice —— 隐私说明，只在它的弹窗打开时（enabled）才去读；整个会话读一次。 */
export function usePrivacyNotice(enabled: boolean): ResourceResult<LegalNotice> {
  return useResource(enabled ? "privacy-notice" : null, loadPrivacyNotice);
}
