import { useEffect, useRef, useState } from 'preact/hooks';
import { invoke, lithe, cancelled } from './bridge.ts';
import { errorMessage, t, locale } from './i18n.ts';
import { bytes } from './model.ts';
import { Icon } from './icons.tsx';
import { VirtualList } from './VirtualList.tsx';

interface Entry { name: string; kind: 'directory' | 'file' | 'symlink' | 'special'; size: number; modified: number | null }
interface Listing { path: string; entries: Entry[] }
const join = (path: string, name: string) => `${path.replace(/\/$/, '')}/${name}`;
const MAX_FILE = 256 * 1024 * 1024;

export function ContainerFiles({ id }: { id: string }) {
  const [path, setPath] = useState('/');
  const [draft, setDraft] = useState('/');
  const [listing, setListing] = useState<Listing | null>(null);
  const [selected, setSelected] = useState('');
  const [revision, setRevision] = useState(0);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [replace, setReplace] = useState<{ name: string; answer(value: boolean): void } | null>(null);
  const confirmation = useRef<((value: boolean) => void) | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const active = useRef<AbortController | null>(null);
  const inFlight = useRef(false);
  const mounted = useRef(true);
  useEffect(() => () => { mounted.current = false; active.current?.abort(); confirmation.current?.(false); }, []);
  useEffect(() => { if (replace) dialog.current?.showModal(); }, [replace]);
  useEffect(() => {
    const controller = new AbortController(); setLoading(true); setError(''); setListing(null); setSelected('');
    void invoke<Listing>('files.list', { id, path }, controller.signal).result.then(value => { if (!controller.signal.aborted) setListing(value); })
      .catch(failure => { if (!controller.signal.aborted) setError(errorMessage(failure)); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [id, path, revision]);
  const navigate = (next: string) => { setPath(next); setDraft(next); };
  const upload = async (files: File[]) => {
    if (inFlight.current || !files.length) return;
    inFlight.current = true;
    const controller = new AbortController(); active.current = controller; setBusy(true); setError('');
    try {
      for (const file of files) {
        if (file.size > MAX_FILE) throw new Error(t('docker.filesLimit'));
        if (mounted.current) setProgress(0);
        if (listing?.entries.some(entry => entry.name === file.name)) {
          const approved = await new Promise<boolean>(resolve => {
            const answer = (value: boolean) => { confirmation.current = null; if (mounted.current) setReplace(null); resolve(value); };
            confirmation.current = answer; setReplace({ name: file.name, answer });
          });
          if (!approved) continue;
        }
        const { transfer } = await invoke<{ transfer: string }>('files.uploadBegin', { id, path, name: file.name, size: file.size }, controller.signal).result;
        try {
          for (let offset = 0; offset < file.size; offset += 192 * 1024) {
            if (controller.signal.aborted) throw new DOMException('Cancelled', 'AbortError');
            const chunk = new Uint8Array(await file.slice(offset, offset + 192 * 1024).arrayBuffer());
            let binary = ''; for (let i = 0; i < chunk.length; i += 8192) binary += String.fromCharCode(...chunk.subarray(i, i + 8192));
            await invoke('files.uploadChunk', { transfer, offset, data: btoa(binary) }, controller.signal).result;
            if (mounted.current) setProgress(Math.round((offset + chunk.length) / Math.max(1, file.size) * 80));
          }
          await invoke('files.uploadCommit', { transfer }, controller.signal, value => { if (mounted.current) setProgress(value ?? 80); }).result;
        } finally { void invoke('files.uploadAbort', { transfer }).result.catch(() => {}); }
      }
      if (mounted.current) setRevision(value => value + 1);
    } catch (failure) { if (mounted.current && !cancelled(failure)) setError(errorMessage(failure)); }
    finally { if (mounted.current) { setBusy(false); setProgress(0); } active.current = null; inFlight.current = false; }
  };
  const chooseUpload = async () => {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true);
    try {
      const answer = await lithe?.request('dialog.openFile', { title: t('upload') }) as { path: string | null } | undefined;
      if (!answer?.path || !mounted.current) return;
      const name = answer.path.split('/').at(-1)!;
      if (listing?.entries.some(entry => entry.name === name)) {
        const approved = await new Promise<boolean>(resolve => {
          const respond = (value: boolean) => { confirmation.current = null; if (mounted.current) setReplace(null); resolve(value); };
          confirmation.current = respond; setReplace({ name, answer: respond });
        });
        if (!approved || !mounted.current) return;
      }
      const controller = new AbortController(); active.current = controller; setBusy(true); setError(''); setProgress(0);
      await invoke('files.uploadLocal', { id, path, source: answer.path }, controller.signal,
        value => { if (mounted.current) setProgress(value ?? 0); }).result;
      if (mounted.current) setRevision(value => value + 1);
    } catch (failure) { if (mounted.current && !cancelled(failure)) setError(errorMessage(failure)); }
    finally { if (mounted.current) { setBusy(false); setProgress(0); } active.current = null; inFlight.current = false; }
  };
  const download = async () => {
    const entry = listing?.entries.find(item => item.name === selected);
    if (!entry || entry.kind !== 'file' || inFlight.current) return;
    inFlight.current = true; setBusy(true);
    try {
      const answer = await lithe?.request('dialog.saveFile', { title: t('download'), suggestedName: entry.name }) as { path: string | null } | undefined;
      if (!answer?.path || !mounted.current) return;
      const controller = new AbortController(); active.current = controller; setBusy(true); setProgress(0); setError('');
      await invoke('files.download', { id, path: join(path, entry.name), destination: answer.path }, controller.signal,
        value => { if (mounted.current) setProgress(value ?? 0); }).result;
    } catch (failure) { if (mounted.current && !cancelled(failure)) setError(errorMessage(failure)); }
    finally { if (mounted.current) { setBusy(false); setProgress(0); } active.current = null; inFlight.current = false; }
  };
  return <div class={`file-browser ${dragging ? 'is-dragging' : ''}`} onDragOver={event => {
    if (!event.dataTransfer?.types.includes('Files') || busy) return;
    event.preventDefault(); event.dataTransfer.dropEffect = 'copy'; setDragging(true);
  }} onDragLeave={event => { if (!event.currentTarget.contains(event.relatedTarget as Node)) setDragging(false); }} onDrop={event => {
    event.preventDefault(); setDragging(false); if (busy) return;
    const items = Array.from(event.dataTransfer?.items ?? []);
    if (items.some(item => item.webkitGetAsEntry?.()?.isDirectory)) { setError(t('foldersUnsupported')); return; }
    void upload(Array.from(event.dataTransfer?.files ?? []));
  }}>
    <div class="file-toolbar"><button class="icon-button" title={t('parentDirectory')} aria-label={t('parentDirectory')} disabled={busy || path === '/'} onClick={() => navigate(path.replace(/\/[^/]+\/?$/, '') || '/')}><Icon name="up"/></button>
      <form onSubmit={event => { event.preventDefault(); if (!busy && draft.startsWith('/')) navigate(draft); }}><input aria-label={t('containerPath')} value={draft} disabled={busy} onInput={event => setDraft(event.currentTarget.value)}/></form>
      <button class="icon-button" title={t('refresh')} aria-label={t('refresh')} disabled={busy} onClick={() => setRevision(value => value + 1)}><Icon name="refresh"/></button>
      <button class="outlined" disabled={busy} onClick={() => void chooseUpload()}><Icon name="upload" size={14}/>{t('upload')}</button>
      <button class="outlined" disabled={busy || listing?.entries.find(entry => entry.name === selected)?.kind !== 'file'} onClick={() => void download()}><Icon name="pull" size={14}/>{t('download')}</button>
    </div>
    {busy && <div class="transfer-progress"><progress max="100" value={progress}/><span>{progress}%</span><button onClick={() => { active.current?.abort(); confirmation.current?.(false); }}>{t('cancel')}</button></div>}
    <div class="file-hint"><Icon name="upload" size={13}/>{t('dropToUpload')}</div>
    {error && <div class="error" role="alert">{error}</div>}
    {loading ? <div class="empty">{t('loading')}</div> : listing && <VirtualList items={listing.entries} label={t('files')} height={30} itemKey={entry => entry.name} selected={selected} onSelect={entry => setSelected(entry.name)}
      header={<div class="file-row file-header"><span>{t('name')}</span><span>{t('modified')}</span><span>{t('size')}</span><span>{t('kind')}</span></div>}
      render={entry => <div class="file-row" onDblClick={() => { if (!busy && entry.kind === 'directory') navigate(join(path, entry.name)); }}>
        <span class="file-name"><Icon name={entry.kind === 'directory' ? 'folder' : 'file'} size={16}/><button disabled={busy} title={entry.name} onClick={event => { if (entry.kind === 'directory') { event.stopPropagation(); navigate(join(path, entry.name)); } }}>{entry.name}</button></span>
        <span>{entry.modified === null ? '—' : new Date(entry.modified * 1000).toLocaleString(locale, { dateStyle: 'short', timeStyle: 'short' })}</span><span>{entry.kind === 'file' ? bytes(entry.size) : '—'}</span><span>{t(entry.kind)}</span>
      </div>}/>}
    {dragging && <div class="file-drop"><Icon name="upload" size={30}/>{t('dropToUpload')}</div>}
    {replace && <dialog class="dialog" ref={dialog} onCancel={event => { event.preventDefault(); replace.answer(false); }}><div class="dialog-body">{t('overwriteFile', { name: replace.name })}</div><footer><button onClick={() => replace.answer(false)}>{t('cancel')}</button><button class="danger outlined" onClick={() => replace.answer(true)}>{t('replace')}</button></footer></dialog>}
  </div>;
}
