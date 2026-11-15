export interface Violation {
  /** 相对扫描根目录的正斜杠路径。 */
  file: string;
  /** 从 1 计的行号。 */
  line: number;
  rule: string;
  snippet: string;
}

export const RULES: Record<string, string>;

export function checkTokens(options: { root: string }): Violation[];

/** tokens.css 里值是阴影列表的令牌 -> 能否写进 text-shadow。 */
export function shadowTokens(tokensCss: string): Map<string, boolean>;
