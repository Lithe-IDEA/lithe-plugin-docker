import { render } from 'preact';
import type { ComponentChildren } from 'preact';
import { useCallback, useEffect, useRef, useState } from 'preact/hooks';
import { cancelled, invoke, restoreState, saveState } from './bridge.ts';
import { ResourceDetails } from './details.tsx';
import { errorMessage, t } from './i18n.ts';
import type { Message } from './i18n.ts';
import { bytes, containerRows, resourceKey, resourceName, runParams, sections, visibleResources } from './model.ts';
import type { Container, Contexts, Engine, Events, Resource, Section, Usage } from './types.ts';
import { VirtualList } from './VirtualList.tsx';
import { Icon } from './icons.tsx';
import { ImageLogo } from './ImageLogo.tsx';
import { ResizeHandle } from './ResizeHandle.tsx';
import { ColumnHandle } from './ColumnHandle.tsx';

type Action = 'start' | 'stop' | 'restart' | 'pause' | 'unpause' | 'kill' | 'remove';
type Modal = { type: 'run'; image?: string } | { type: 'pull' } | { type: 'prune' }
  | { type: 'action'; action: Action; id?: string; name: string; project?: string };
interface Layout { sidebar?: number; details?: number; detailsHeight?: number; nameColumn?: number; imageColumn?: number }
interface PageState { section: Section; selected?: string; collapsed?: string[]; runningOnly?: boolean; layout?: Layout; sampleInterval?: number }
const states: Record<string, Message> = { running: 'statusRunning', exited: 'statusExited', paused: 'statusPaused', created: 'statusCreated', restarting: 'statusRestarting', dead: 'statusDead', removing: 'statusRemoving' };

function App() {
  const [section, setSection] = useState<Section>('containers');
  const [selected, setSelected] = useState<string>();
  const [collapsed, setCollapsed] = useState(new Set<string>());
  const [runningOnly, setRunningOnly] = useState(false);
  const [sampleInterval, setSampleInterval] = useState(2000);
  const [search, setSearch] = useState('');
  const [engine, setEngine] = useState<Engine | null>(null);
  const [resources, setResources] = useState<Resource[]>([]);
  const [usage, setUsage] = useState<Usage | null>(null);
  const [error, setError] = useState('');
  const [liveError, setLiveError] = useState(false);
  const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0);
  const [connectionRevision, setConnectionRevision] = useState(0);
  const [modal, setModal] = useState<Modal | null>(null);
  const [ready, setReady] = useState(false);
  const [visible, setVisible] = useState(document.visibilityState !== 'hidden');
  const [layout, setLayout] = useState<Layout>({});
  const app = useRef<HTMLDivElement>(null);
  const sidebar = useRef<HTMLElement>(null);
  const resourceLayout = useRef<HTMLDivElement>(null);
  const details = useRef<HTMLElement>(null);
  const list = useRef<HTMLElement>(null);
  const refresh = useRef<AbortController | undefined>(undefined);
  useEffect(() => {
    void restoreState<PageState>().then(state => {
      if (!state) return;
      if (sections.includes(state.section)) setSection(state.section);
      if (typeof state.selected === 'string') setSelected(state.selected);
      if (Array.isArray(state.collapsed)) setCollapsed(new Set(state.collapsed.filter(value => typeof value === 'string')));
      setRunningOnly(state.runningOnly === true);
      if ([1000, 2000, 5000, 10000].includes(state.sampleInterval ?? 0)) setSampleInterval(state.sampleInterval!);
      const saved = state.layout;
      if (saved && typeof saved === 'object') setLayout(Object.fromEntries(
        ['sidebar', 'details', 'detailsHeight', 'nameColumn', 'imageColumn'].filter(key => {
          const value = saved[key as keyof Layout];
          return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 10000;
        }).map(key => [key, saved[key as keyof Layout]])));
    }).catch(() => {}).finally(() => setReady(true));
    const visibility = () => setVisible(document.visibilityState !== 'hidden');
    document.addEventListener('visibilitychange', visibility);
    return () => { document.removeEventListener('visibilitychange', visibility); refresh.current?.abort(); };
  }, []);
  useEffect(() => {
    if (ready) saveState({ section, selected, collapsed: [...collapsed].slice(0, 500), runningOnly, layout, sampleInterval });
  }, [ready, section, selected, collapsed, runningOnly, layout, sampleInterval]);

  const load = useCallback(async (initial = false) => {
    refresh.current?.abort();
    const controller = new AbortController(); refresh.current = controller;
    setError(''); if (initial) setLoading(true);
    try {
      const info = await invoke<Engine>('engine.info', {}, controller.signal).result;
      if (controller.signal.aborted) return;
      setEngine(info);
      if (!info.reachable || section === 'connection') { setResources([]); return; }
      if (section === 'system') setUsage(await invoke<Usage>('system.df', {}, controller.signal).result);
      else setResources(await invoke<Resource[]>(`${section}.list`, {}, controller.signal).result);
    } catch (failure) { if (!controller.signal.aborted && !cancelled(failure)) setError(errorMessage(failure)); }
    finally { if (!controller.signal.aborted) setLoading(false); }
  }, [section]);
  useEffect(() => {
    if (ready && visible) void load(true);
    return () => refresh.current?.abort();
  }, [ready, visible, load, revision]);

  useEffect(() => {
    if (!ready || !visible || !engine?.reachable) return;
    const controller = new AbortController(); setLiveError(false);
    void (async () => {
      let cursor: string | null = null;
      try {
        while (!controller.signal.aborted) {
          const events: Events = await invoke<Events>('events.next', { since: cursor, wait: 20000 }, controller.signal).result;
          cursor = events.cursor;
          if (events.events.length && section !== 'system' && section !== 'connection') await load();
        }
      } catch (failure) { if (!controller.signal.aborted && !cancelled(failure)) setLiveError(true); }
    })();
    return () => controller.abort();
  }, [ready, visible, engine?.reachable, connectionRevision, load, section, revision]);

  function chooseSection(next: Section) { setSection(next); setSelected(undefined); setResources([]); setSearch(''); }
  const items = visibleResources(resources, search, section === 'containers' && runningOnly);
  const item = resources.find(resource => resourceKey(resource) === selected);
  function action(action: Action) {
    if (!item) return;
    setModal({ type: 'action', action, id: resourceKey(item), name: resourceName(item) });
  }
  const connected = engine?.reachable === true;
  const runningCount = resources.filter(resource => 'state' in resource && resource.state === 'running').length;
  const header = <div class="resource-grid table-header"><span>{t('name')}<ColumnHandle list={list} column={0} label={t('resizeNameColumn')}
    onCommit={nameColumn => setLayout(previous => ({ ...previous, nameColumn }))}/></span><span>{t(section === 'containers' ? 'image' : section === 'images' ? 'size' : 'driver')}<ColumnHandle list={list} column={1} label={t('resizeImageColumn')}
    onCommit={imageColumn => setLayout(previous => ({ ...previous, imageColumn }))}/></span><span>{t(section === 'containers' ? 'state' : 'usedBy')}</span></div>;
  const hasResources = !['system', 'connection'].includes(section);
  const filters = <div class="toolbar-filters"><div class="search-field"><Icon name="search"/><input type="search" placeholder={t('search')} aria-label={t('search')} value={search} onInput={event => setSearch(event.currentTarget.value)}/></div>
    {section === 'containers' && <label class="check"><input type="checkbox" checked={runningOnly} onChange={event => setRunningOnly(event.currentTarget.checked)}/>{t('runningOnly')}</label>}
    <span class="resource-total">{section === 'containers' ? t('containerCount', { count: items.length, running: runningCount }) : t('resourceCount', { count: items.length })}</span></div>;
  return <div class="app" ref={app}>
    <nav class="sidebar" ref={sidebar} style={layout.sidebar === undefined ? undefined : { '--pane-width': `${layout.sidebar}px` }} aria-label="Docker"><div class="brand"><img src="../icons/docker.svg" alt=""/><div><strong>Docker</strong><span>{engine?.endpoint?.context ?? t('engine')}</span></div></div>
      {sections.map(value => <button class={section === value ? 'active' : ''} onClick={() => chooseSection(value)} title={t(value)}>
        <Icon name={value}/><span>{t(value)}</span>
      </button>)}
      <div class="spacer"/><button class="engine-status" onClick={() => chooseSection('connection')} title={engine?.endpoint?.socket}>
        <span class={`dot ${connected ? 'running' : 'exited'}`}/><span>{connected ? t('connected') : t('disconnected')}</span>
      </button>
      <ResizeHandle pane={sidebar} container={app} kind="sidebar" label={t('resizeNavigation')}
        onCommit={sidebar => setLayout(previous => ({ ...previous, sidebar }))}/>
    </nav>
    <main><header class="toolbar"><h1>{t(section)}</h1>
      <span class="endpoint" title={engine?.endpoint?.socket}><span class={`dot ${connected ? 'running' : 'exited'}`}/>{engine?.endpoint?.context ?? engine?.info?.Name}</span>
      {connected && hasResources && filters}
      <span class="spacer"/>
      <button class="icon-button" aria-label={t('refresh')} disabled={loading || !!modal} onClick={() => setRevision(value => value + 1)} title={t('refresh')}><Icon name="refresh"/></button>
      {(section === 'containers' || section === 'images') && <button class="primary" disabled={!connected || !!modal} onClick={() => setModal(section === 'containers' ? { type: 'run' } : { type: 'pull' })}><Icon name={section === 'containers' ? 'run' : 'pull'}/>{t(section === 'containers' ? 'run' : 'pull')}</button>}
      {section === 'system' && <button class="danger outlined" disabled={!connected || !!modal} onClick={() => setModal({ type: 'prune' })}><Icon name="remove"/>{t('prune')}</button>}
    </header>
      {error && <div role="alert" class="error">{error}</div>}
      {liveError && <div class="notice" role="status">{t('liveFailed')}<button onClick={() => setRevision(value => value + 1)}>{t('retry')}</button></div>}
      {!visible && <div class="notice">{t('livePaused')}</div>}
      {section === 'connection' ? <Connection key={connectionRevision} engine={engine} changed={() => { setConnectionRevision(value => value + 1); setEngine(null); setSelected(undefined); setRevision(value => value + 1); }}/>
        : !connected ? <div class="empty disconnected"><img src="../icons/docker.svg" alt=""/><h2>{t('disconnected')}</h2>
          <p>{engine?.error ? errorMessage(engine.error) : t('requiresHost')}</p><p class="muted">{t('unavailable')}</p>
          <button onClick={() => chooseSection('connection')}>{t('connection')}</button>
        </div>
        : section === 'system' ? <DiskUsage usage={usage} engine={engine}/>
          : <><div class={`resource-layout ${item ? 'has-selection' : ''}`} ref={resourceLayout}><section class="resource-list" ref={list} style={{
            '--name-width': layout.nameColumn === undefined ? 'minmax(120px, 1.1fr)' : `${layout.nameColumn}px`,
            '--image-width': layout.imageColumn === undefined ? 'minmax(100px, 1.25fr)' : `${layout.imageColumn}px`,
            '--name-min': `${layout.nameColumn ?? 120}px`, '--image-min': `${layout.imageColumn ?? 100}px`,
          }}>
            {items.length === 0 ? <div class="empty">{loading ? t('loading') : t('empty')}</div>
              : section === 'containers' ? <VirtualList items={containerRows(items as Container[], collapsed)} height={34} label={t('containers')}
                header={header} selected={selected} itemKey={row => row.type === 'group' ? `group:${row.project ?? ''}` : row.item.id}
                onSelect={row => { if (row.type === 'item') setSelected(row.item.id); else setCollapsed(previous => {
                  const next = new Set(previous); const key = row.project ?? ''; if (next.has(key)) next.delete(key); else next.add(key); return next;
                }); }} render={row => row.type === 'group' ? <div class="group-row"><Icon name={collapsed.has(row.project ?? '') ? 'collapsed' : 'expanded'} size={13}/>
                  {row.project && <Icon name="project" size={15}/>}<strong>{row.project ?? t('standalone')}</strong><span class="count">{row.count}</span>
                  {row.project && row.paths.length > 0 && <span class="compose-path" title={row.paths.join('\n')}>{row.paths.join(', ')}</span>}<span class="spacer"/>
                  {row.project && <span class="group-actions" title={t('groupWarning')}>{(['start', 'stop', 'restart', 'remove'] as const).map(action =>
                    <button class={`icon-button ${action === 'start' ? 'positive' : action === 'remove' || action === 'stop' ? 'danger' : ''}`} aria-label={t('confirmAction', { action: t(action), name: row.project! })} title={t('confirmAction', { action: t(action), name: row.project! })} disabled={!!modal} onClick={event => { event.stopPropagation(); setModal({ type: 'action', project: row.project!, name: row.project!, action }); }}><Icon name={action} size={14}/></button>)}</span>}
                </div> : <ResourceRow item={row.item} section={section}/>}/>
                : <VirtualList items={items} label={t(section)} height={34} header={header} selected={selected}
                  itemKey={resourceKey} onSelect={item => setSelected(resourceKey(item))} render={item => <ResourceRow item={item} section={section}/>}/>}
          </section>{item && <section class="detail-panel" ref={details} style={{
            '--pane-width': layout.details === undefined ? '42%' : `${layout.details}px`,
            '--pane-height': layout.detailsHeight === undefined ? '60%' : `${layout.detailsHeight}px`,
          }}>
            <ResizeHandle pane={details} container={resourceLayout} kind="details" label={t('resizeDetails')}
              onCommit={(size, axis) => setLayout(previous => ({ ...previous, [axis === 'x' ? 'details' : 'detailsHeight']: size }))}/>
            <>
              <div class="detail-heading"><strong title={resourceName(item)}>{resourceName(item)}</strong><span class="spacer"/>
                <div class="detail-actions">
                {'state' in item ? <>
                  {(item.state === 'running' ? ['stop', 'restart', 'pause'] as const : item.state === 'paused' ? ['unpause', 'stop'] as const : ['start'] as const).map(value => <button class={value === 'start' || value === 'unpause' ? 'success outlined' : value === 'stop' || value === 'pause' ? 'danger' : ''} disabled={!!modal} onClick={() => action(value)}><Icon name={value} size={14}/>{t(value)}</button>)}
                  {['running', 'paused', 'restarting'].includes(item.state) && <button class="danger" disabled={!!modal} onClick={() => action('kill')}><Icon name="kill" size={14}/>{t('kill')}</button>}
                </> : 'tags' in item ? <button disabled={!!modal} onClick={() => setModal({ type: 'run', image: item.tags[0] ?? item.id })}><Icon name="start" size={14}/>{t('run')}</button> : null}
                <span class="spacer"/>{(!('builtin' in item) || !item.builtin) && <button class="danger icon-button" aria-label={t('remove')} title={t('remove')} disabled={!!modal} onClick={() => action('remove')}><Icon name="remove" size={14}/></button>}
              </div><button class="icon-button" title={t('close')} aria-label={t('close')} onClick={() => setSelected(undefined)}><Icon name="close"/></button>
              </div>{visible && <ResourceDetails key={`${section}:${resourceKey(item)}:${connectionRevision}`} item={item} section={section} revision={revision} sampleInterval={sampleInterval} onSampleInterval={setSampleInterval}/>}
            </>
          </section>}</div></>}
    </main>
    {modal && <ActionDialog modal={modal} section={section} close={() => setModal(null)} done={() => { setModal(null); setRevision(value => value + 1); }}/>}
  </div>;
}

function ResourceRow({ item, section }: { item: Resource; section: Section }) {
  return <div class="resource-grid" title={resourceName(item)}><span class="resource-name">
    <ImageLogo image={'image' in item ? item.image : 'tags' in item ? item.tags[0] ?? '' : ''} fallback={section === 'system' || section === 'connection' ? 'containers' : section}/><span>{resourceName(item)}</span></span>
    <span class="muted">{'image' in item ? item.image : 'size' in item ? bytes(item.size) : item.driver}</span>
    <span>{'state' in item ? <span class={`state-label ${item.state}`}><span class={`dot ${item.state}`}/>{t(states[item.state] ?? 'unknown')}</span>
      : typeof item.containers === 'number' ? item.containers : item.containers.join(', ') || t('unused')}</span>
    {section === 'networks' && 'builtin' in item && item.builtin && <span class="sr-only">{t('builtin')}</span>}
  </div>;
}

function DiskUsage({ usage, engine }: { usage: Usage | null; engine: Engine | null }) {
  return <div class="scroll-content">
    <div class="engine-summary"><strong>{engine?.platform || engine?.info?.Name}</strong><span class="muted">{engine?.version?.Version} · {engine?.info?.OperatingSystem}</span></div>
    {!usage ? <div class="empty">{t('loading')}</div> : <div class="usage-grid">{(['images', 'containers', 'volumes', 'buildCache'] as const).map(key => <section class="usage-card">
      <h2>{t(key)}</h2><strong class="usage-size">{bytes(usage[key].size)}</strong>
      <dl><div><dt>{t('total')}</dt><dd>{usage[key].count}</dd></div><div><dt>{t('active')}</dt><dd>{usage[key].active}</dd></div>
        <div><dt>{t('reclaimable')}</dt><dd>{bytes(usage[key].reclaimable)}</dd></div></dl>
      <meter min={0} max={Math.max(1, usage[key].size)} value={usage[key].reclaimable} aria-label={t('reclaimable')}/>
    </section>)}</div>}
  </div>;
}

function Field({ title, help, children }: { title: string; help?: string; children: ComponentChildren }) {
  return <label class="field"><span>{title}{help && <span class="info" title={help} tabIndex={0} aria-label={help}>ⓘ</span>}</span>{children}</label>;
}

function Connection({ engine, changed }: { engine: Engine | null; changed(): void }) {
  const [contexts, setContexts] = useState<Contexts | null>(null);
  const [choice, setChoice] = useState('');
  const [socket, setSocket] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const abort = useRef(new AbortController());
  useEffect(() => {
    const controller = abort.current;
    void invoke<Contexts>('contexts.list', {}, controller.signal).result.then(result => {
      setContexts(result); setChoice(result.choice.socket ? ':socket' : result.choice.context ?? ''); setSocket(result.choice.socket ?? '');
    }).catch(failure => { if (!controller.signal.aborted && !cancelled(failure)) setError(errorMessage(failure)); });
    return () => controller.abort();
  }, []);
  return <div class="scroll-content connection"><section class="connection-config"><div class="connection-heading"><Icon name="endpoint" size={19}/><h2>{t('connection')}</h2><span class={`connection-badge ${engine?.reachable ? 'connected' : ''}`}><span class={`dot ${engine?.reachable ? 'running' : 'exited'}`}/>{engine?.reachable ? t('connected') : t('disconnected')}</span></div><form onSubmit={async event => {
    event.preventDefault(); setError('');
    if (choice === ':socket' && !socket.startsWith('/')) { setError(t('socketRequired')); return; }
    setBusy(true);
    try { await invoke('contexts.use', choice === ':socket' ? { socket } : choice ? { context: choice } : {}, abort.current.signal).result; changed(); }
    catch (failure) { if (!cancelled(failure)) setError(errorMessage(failure)); }
    finally { setBusy(false); }
  }}>
    <Field title={t('context')} help={t('connectionHelp')}><select value={choice} disabled={busy || !contexts} onChange={event => setChoice(event.currentTarget.value)}>
      <option value="">{t('automatic')}</option>{contexts?.contexts.map(context => <option value={context.name}>{context.name} · {context.host}</option>)}
      <option value=":socket">{t('customSocket')}</option></select></Field>
    {choice === ':socket' && <Field title={t('socket')}><input required value={socket} disabled={busy} onInput={event => setSocket(event.currentTarget.value)} placeholder="/var/run/docker.sock"/></Field>}
    {error && <p role="alert" class="error">{error}</p>}
    <button class="primary" type="submit" disabled={busy || !contexts}>{busy ? t('working') : t('apply')}</button>
  </form></section><section class="connection-info"><div class="connection-heading"><h2>{t('engine')}</h2><span class="endpoint">{engine?.info?.OperatingSystem ?? 'Docker'}</span></div><dl>
    <div><dt>{t('endpoint')}</dt><dd>{engine?.endpoint?.socket ?? contexts?.endpoint?.socket ?? '—'}</dd></div>
    <div><dt>{t('currentContext')}</dt><dd>{contexts?.current ?? 'default'}</dd></div>
    <div><dt>{t('source')}</dt><dd>{engine?.endpoint?.source ?? contexts?.endpoint?.source ?? '—'}</dd></div>
    <div><dt>{t('version')}</dt><dd>{engine?.version?.Version ?? '—'}</dd></div>
    <div><dt>{t('apiVersion')}</dt><dd>{engine?.version?.ApiVersion ?? '—'}</dd></div>
    <div><dt>{t('operatingSystem')}</dt><dd>{engine?.info?.OperatingSystem ?? '—'}</dd></div>
    <div><dt>{t('architecture')}</dt><dd>{engine?.version?.Arch ?? '—'}</dd></div>
    <div><dt>{t('cpus')}</dt><dd>{engine?.info?.NCPU ?? '—'}</dd></div>
    <div><dt>{t('memory')}</dt><dd>{bytes(engine?.info?.MemTotal)}</dd></div>
    <div><dt>{t('storageDriver')}</dt><dd>{engine?.info?.Driver ?? '—'}</dd></div>
  </dl></section></div>;
}

function ActionDialog({ modal, section, close, done }: { modal: Modal; section: Section; close(): void; done(): void }) {
  const ref = useRef<HTMLDialogElement>(null);
  const abort = useRef(new AbortController());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [percentage, setPercentage] = useState<number>();
  useEffect(() => { const dialog = ref.current!; dialog.showModal(); return () => { abort.current.abort(); dialog.close(); }; }, []);
  const title = modal.type === 'run' ? t('run') : modal.type === 'pull' ? t('pull') : modal.type === 'prune' ? t('prune') : t(modal.action);
  const submit = async (form: HTMLFormElement) => {
    const data = new FormData(form); setError('');
    let fn: string; let params: object;
    try {
      if (modal.type === 'run') { fn = 'containers.create'; params = runParams(data); }
      else if (modal.type === 'pull') { fn = 'images.pull'; params = { image: data.get('image') }; }
      else if (modal.type === 'prune') {
        fn = 'system.prune'; params = Object.fromEntries(['containers', 'networks', 'volumes', 'allVolumes', 'images', 'allImages', 'buildCache'].map(key => [key, data.has(key)]));
      } else if (modal.project) { fn = 'compose.action'; params = { project: modal.project, action: modal.action }; }
      else {
        fn = modal.action === 'remove' ? `${section}.remove` : 'containers.action';
        params = { id: modal.id, name: modal.id, action: modal.action, force: data.has('force'), volumes: data.has('volumes') };
      }
    } catch (failure) { setError(t((failure as Error).message as Message)); return; }
    setBusy(true);
    try { await invoke(fn, params, abort.current.signal, setPercentage).result; done(); }
    catch (failure) { if (!cancelled(failure)) setError(errorMessage(failure)); }
    finally { setBusy(false); }
  };
  return <dialog ref={ref} class="dialog" aria-labelledby="dialog-title" onCancel={event => { event.preventDefault(); abort.current.abort(); close(); }}>
    <form onSubmit={event => { event.preventDefault(); void submit(event.currentTarget); }}>
      <header><h2 id="dialog-title">{title}</h2></header><div class="dialog-body">
        <fieldset disabled={busy}>
          {modal.type === 'run' && <>
            <Field title={t('imageReference')}><input name="image" required value={modal.image} placeholder="nginx:alpine" autoFocus/></Field>
            <Field title={t('containerName')}><input name="name"/></Field>
            <Field title={t('portMappings')} help={t('portHelp')}><textarea name="ports" rows={2} placeholder="8080:80"/></Field>
            <Field title={t('environment')} help={t('environmentHelp')}><textarea name="environment" rows={2} spellcheck={false}/></Field>
            <Field title={t('volumeMappings')} help={t('volumeHelp')}><textarea name="volumes" rows={2} spellcheck={false}/></Field>
            <Field title={t('command')} help={t('commandHelp')}><input name="command" spellcheck={false}/></Field>
            <div class="form-grid"><Field title={t('restartPolicy')}><select name="restart"><option value="no">{t('restartNo')}</option><option value="always">{t('restartAlways')}</option><option value="unless-stopped">{t('restartUnless')}</option><option value="on-failure">{t('restartFailure')}</option></select></Field>
              <Field title={t('network')}><input name="network"/></Field></div>
            <p class="notice">{t('runWarning')}</p>
          </>}
          {modal.type === 'pull' && <Field title={t('imageReference')}><input name="image" required placeholder="nginx:alpine" autoFocus/></Field>}
          {modal.type === 'prune' && <>
            <p class="notice">{t('cleanWarning')}</p>
            {(['containers', 'networks', 'volumes', 'allVolumes', 'images', 'allImages', 'buildCache'] as const).map((key, index) => <label class="check prune-check">
              <input type="checkbox" name={key}/>{t((['stoppedContainers', 'unusedNetworks', 'anonymousVolumes', 'allVolumes', 'danglingImages', 'allImages', 'buildCache'] as const)[index])}
            </label>)}
          </>}
          {modal.type === 'action' && <><p class="confirm-name">{modal.action === 'remove' ? t('removeWarning', { name: modal.name }) : t('confirmAction', { name: modal.name, action: t(modal.action) })}</p>
            {modal.project && <p class="notice">{t('groupWarning')}</p>}
            {['stop', 'restart', 'kill'].includes(modal.action) && <p class="notice">{t('interruptWarning')}</p>}
            {modal.action === 'remove' && !modal.project && section !== 'networks' && <label class="check"><input type="checkbox" name="force"/>{t('force')}</label>}
            {modal.action === 'remove' && section === 'containers' && !modal.project && <label class="check"><input type="checkbox" name="volumes"/>{t('removeVolumes')}</label>}
          </>}
        </fieldset>
        {error && <p class="error" role="alert">{error}</p>}
        {busy && <div class="progress"><span>{t('working')}</span><progress max={100} value={percentage}/></div>}
      </div><footer><button type="button" onClick={() => { abort.current.abort(); close(); }}>{t('cancel')}</button>
        <button class={modal.type === 'prune' || modal.type === 'action' && ['remove', 'kill'].includes(modal.action) ? 'danger primary' : 'primary'} type="submit" disabled={busy}>
          {modal.type === 'run' ? t('create') : title}</button></footer>
    </form>
  </dialog>;
}

render(<App/>, document.getElementById('app')!);
