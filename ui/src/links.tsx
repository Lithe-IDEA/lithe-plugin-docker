import { useState } from 'preact/hooks';
import { invoke } from './bridge.ts';
import { errorMessage } from './i18n.ts';

export function webLinks(text: string): { from: number; to: number; url: string }[] {
  return [...text.matchAll(/https?:\/\/[^\s<>"'`]+/g)].flatMap(match => {
    const url = match[0].replace(/[.,;!?)}\]]+$/, '');
    try {
      const parsed = new URL(url);
      return parsed.hostname && !parsed.username && !parsed.password
        ? [{ from: match.index, to: match.index + url.length, url }] : [];
    } catch { return []; }
  });
}

export async function openWebLink(url: string): Promise<void> {
  await invoke('browser.open', { url }).result;
}

export function WebLink({ url, children }: { url: string; children?: string }) {
  const [error, setError] = useState('');
  return <><a href={url} class="web-link" onClick={event => {
    event.preventDefault(); setError(''); void openWebLink(url).catch(failure => setError(errorMessage(failure)));
  }}>{children ?? url}</a>{error && <span class="link-error" role="alert">{error}</span>}</>;
}

export function LinkedText({ value }: { value: string }) {
  let end = 0;
  const content = webLinks(value).flatMap(link => {
    const prefix = value.slice(end, link.from); end = link.to;
    return [prefix, <WebLink url={link.url}/>];
  });
  return <>{content}{value.slice(end)}</>;
}
