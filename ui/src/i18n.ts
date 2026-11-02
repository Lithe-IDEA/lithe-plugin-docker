import en from '../../shared/i18n/en-US.json';
import zh from '../../shared/i18n/zh-CN.json';
import { lithe } from './bridge.ts';

export type Message = keyof typeof en;
export const locale = lithe?.context.locale ?? navigator.language;
export function t(key: Message, values: Record<string, string | number> = {}): string {
  const text: string = /^zh/i.test(locale) ? zh[key] : en[key];
  return text.replace(/\{(\w+)\}/g, (match, name: string) => String(values[name] ?? match));
}
export function errorMessage(error: unknown): string {
  const value = error as { code?: string; pluginErrorCode?: string; message?: string };
  const key = value?.pluginErrorCode ?? value?.code ?? '';
  if (Object.hasOwn(en, key)) return t(key as Message);
  // Engine diagnostics are upstream text and retain their original wording.
  return value?.message || t('failed');
}
