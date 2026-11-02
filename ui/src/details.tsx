import { useEffect, useRef, useState } from 'preact/hooks';
import { cancelled, invoke } from './bridge.ts';
import { errorMessage, t } from './i18n.ts';
import { bytes } from './model.ts';
import type { Logs, LogLine, Resource, Section, Stats } from './types.ts';
import { TextViewport } from './TextViewport.tsx';
import { Icon } from './icons.tsx';
import { ContainerFiles } from './ContainerFiles.tsx';
import { ResourceSummary } from './ResourceSummary.tsx';

export function TextView({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState('');
  const [wrap, setWrap] = useState(false);
  return <div class="text-view"><div class="subtoolbar"><span>{label}</span><span class="spacer"/>
    <button class={`icon-button ${wrap ? 'toggled' : ''}`} aria-label={t('wrapLines')} title={t('wrapLines')} aria-pressed={wrap} onClick={() => setWrap(!wrap)}><Icon name="wrap"/></button>
    <button onClick={async () => {
      try {
        const copy = (event: ClipboardEvent) => { event.clipboardData?.setData('text/plain', value); event.preventDefault(); };
        document.addEventListener('copy', copy, { once: true });
        const success = document.execCommand('copy');
        document.removeEventListener('copy', copy);
        if (!success) throw new Error();
        setCopied(true);
      } catch { setError(t('copyFailed')); }
    }}>{copied ? t('copied') : t('copy')}</button></div>
    {error && <p class="error">{error}</p>}
    <TextViewport value={value} label={label} wrap={wrap} language="json"/>
  </div>;
}

export function LogView({ id }: { id: string }) {
  const [lines, setLines] = useState<LogLine[]>([]);
  const [follow, setFollow] = useState(true);
  const [timestamps, setTimestamps] = useState(false);
  const [wrap, setWrap] = useState(false);
  const [error, setError] = useState('');
  const [truncated, setTruncated] = useState(false);
  const [ended, setEnded] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const cursor = useRef<string | null>(null);
  const initial = useRef(false);
  useEffect(() => {
    const controller = new AbortController();
    setError(''); setEnded(false);
    void (async () => {
      try {
        if (!initial.current) {
          const result = await invoke<Logs>('containers.logs', { id, tail: 500 }, controller.signal).result;
          setLines(result.lines); cursor.current = result.cursor;
          setTruncated(result.truncated); initial.current = true;
        }
        while (follow && !controller.signal.aborted) {
          const result = await invoke<Logs>('containers.logs', { id, follow: true, since: cursor.current, wait: 15000 }, controller.signal).result;
          cursor.current = result.cursor;
          setLines(previous => {
            const next = [...previous, ...result.lines];
            // Both byte and row limits bound retained output and text-copy cost.
            let size = 0; let first = next.length;
            while (first > 0 && next.length - first < 4000 && size + next[first - 1].text.length < 750000) {
              size += next[--first].text.length;
            }
            if (first) setTruncated(true);
            return next.slice(first);
          });
          if (result.ended) { setEnded(true); break; }
        }
      } catch (failure) { if (!controller.signal.aborted && !cancelled(failure)) setError(errorMessage(failure)); }
    })();
    return () => controller.abort();
  }, [id, follow, attempt]);
  return <div class="text-view"><div class="subtoolbar">
    <label class="check"><input type="checkbox" checked={follow} onChange={event => setFollow(event.currentTarget.checked)}/>{t('follow')}</label>
    <label class="check"><input type="checkbox" checked={timestamps} onChange={event => setTimestamps(event.currentTarget.checked)}/>{t('timestamps')}</label>
    {ended && <span class="log-ended"><span class="dot"/>{t('logsEnded')}</span>}
    <span class="spacer"/><button class={`icon-button ${wrap ? 'toggled' : ''}`} aria-label={t('wrapLines')} title={t('wrapLines')} aria-pressed={wrap} onClick={() => setWrap(!wrap)}><Icon name="wrap"/></button>
    <button class="icon-button" title={t('refresh')} aria-label={t('refresh')} onClick={() => { initial.current = false; setAttempt(attempt + 1); }}><Icon name="refresh"/></button>
  </div>
    {truncated && <div class="notice">{t('truncated')}</div>}
    {error && <div class="error">{error}<button onClick={() => setAttempt(attempt + 1)}>{t('retry')}</button></div>}
    <TextViewport value={lines.map(line => `${timestamps ? line.time + ' ' : ''}${line.text}`).join('\n')}
      timestampLengths={timestamps ? lines.map(line => line.time.length + 1) : []}
      label={t('logs')} wrap={wrap} follow={follow} showSearch language="logs"/>
  </div>;
}

function Sparkline({ values, label, format }: { values: number[]; label: string; format(value: number): string }) {
  const [hover, setHover] = useState<number | null>(null);
  const ceiling = Math.max(1, ...values);
  const chartX = (index: number) => (60 - values.length + index) * 300 / 59;
  const points = values.map((value, index) => `${chartX(index)},${68 - value / ceiling * 62}`);
  const index = hover === null ? values.length - 1 : Math.min(values.length - 1, hover);
  const x = chartX(index);
  return <div class="sparkline" tabIndex={0} aria-label={`${label}: ${format(values[index] ?? 0)}`}
    onKeyDown={event => {
      if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
        event.preventDefault(); setHover(Math.max(0, Math.min(values.length - 1, index + (event.key === 'ArrowLeft' ? -1 : 1))));
      }
    }} onMouseLeave={() => setHover(null)}>
    <div class="spark-title"><span>{label}</span><strong>{format(values[index] ?? 0)}</strong></div>
    <svg viewBox="0 0 300 74" preserveAspectRatio="none" role="img" aria-label={label}
      onMouseMove={event => {
        const rect = event.currentTarget.getBoundingClientRect();
        setHover(Math.max(0, Math.min(values.length - 1, Math.round((event.clientX - rect.left) / rect.width * 59) - (60 - values.length))));
      }}>
      <path class="spark-grid" d="M0 68H300 M0 36H300 M0 6H300"/>
      <path class="spark-fill" d={`M${chartX(0)} 68L${points.join('L')}L300 68Z`}/>
      <polyline class="spark-line" points={points.join(' ')}/>
      {values.length === 1 && <circle class="spark-dot" cx={x} cy={68 - (values[0] ?? 0) / ceiling * 62} r="2"/>}
      {hover !== null && <><path class="spark-grid" d={`M${x} 0V74`}/><circle class="spark-dot" cx={x} cy={68 - (values[index] ?? 0) / ceiling * 62} r="4"/></>}
    </svg>
  </div>;
}

export function StatsView({ id, state, interval, onInterval }: { id: string; state: string; interval: number; onInterval(value: number): void }) {
  const [samples, setSamples] = useState<(Stats & { sampledAt: number })[]>([]);
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    setError('');
    if (!['running', 'paused'].includes(state)) { setSamples([]); return () => controller.abort(); }
    const read = async () => {
      const started = performance.now();
      try {
        const result = await invoke<Stats>('containers.stats', { id }, controller.signal).result;
        if (controller.signal.aborted) return;
        setSamples(previous => [...previous.slice(-59), { ...result, sampledAt: performance.now() }]);
        timer = setTimeout(() => void read(), Math.max(0, interval - (performance.now() - started)));
      } catch (failure) { if (!controller.signal.aborted && !cancelled(failure)) setError(errorMessage(failure)); }
    };
    void read();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [id, state, attempt, interval]);
  const current = samples.at(-1);
  const rate = (index: number, key: 'networkReceived' | 'networkSent' | 'blockRead' | 'blockWritten') => {
    if (index === 0) return 0;
    const elapsed = (samples[index].sampledAt - samples[index - 1].sampledAt) / 1000;
    return elapsed > 0 ? Math.max(0, samples[index][key] - samples[index - 1][key]) / elapsed : 0;
  };
  return <div class="stats-view">
    <div class="subtoolbar stats-toolbar"><span class="state-label"><span class={`dot ${state}`}/>{t(state === 'running' ? 'statusRunning' : state === 'paused' ? 'statusPaused' : 'statusExited')}</span><span class="spacer"/>
      <label class="sample-control">{t('sampleInterval')}<select aria-label={t('sampleInterval')} value={interval} onChange={event => onInterval(Number(event.currentTarget.value))}>
        {[1000, 2000, 5000, 10000].map(value => <option value={value}>{t('seconds', { count: value / 1000 })}</option>)}</select></label>
    </div>
    {error && <div class="error">{error}<button onClick={() => setAttempt(attempt + 1)}>{t('retry')}</button></div>}
    {!current ? <div class="empty">{t(['running', 'paused'].includes(state) ? 'loading' : 'statsStopped')}</div> : <>
      <div class="sparks"><Sparkline values={samples.map(item => item.cpuPercent)} label={t('cpu')} format={value => `${value.toFixed(1)}%`}/>
        <Sparkline values={samples.map(item => item.memoryUsed)} label={t('memory')} format={bytes}/>
        <Sparkline values={samples.map((_, index) => rate(index, 'networkReceived') + rate(index, 'networkSent'))} label={t('networkRate')} format={value => `${bytes(value)}/s`}/>
        <Sparkline values={samples.map((_, index) => rate(index, 'blockRead') + rate(index, 'blockWritten'))} label={t('diskRate')} format={value => `${bytes(value)}/s`}/></div>
      <div class="stats-footer"><span>{t('memoryLimit')}: {bytes(current.memoryLimit)}</span><span>{t('pids')}: {current.processes ?? '—'}</span></div>
    </>}
  </div>;
}

function RemoteText({ fn, params, label }: { fn: string; params: object; label: string }) {
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const controller = new AbortController(); setError(''); setText(null);
    void invoke<unknown>(fn, params, controller.signal).result.then(result => setText(JSON.stringify(result, null, 2)))
      .catch(failure => { if (!controller.signal.aborted && !cancelled(failure)) setError(errorMessage(failure)); });
    return () => controller.abort();
  }, [fn, JSON.stringify(params), attempt]);
  return error ? <div class="error">{error}<button onClick={() => setAttempt(attempt + 1)}>{t('retry')}</button></div>
    : text === null ? <div class="empty">{t('loading')}</div> : <TextView value={text} label={label}/>;
}

export function ResourceDetails({ item, section, revision, sampleInterval, onSampleInterval }: { item: Resource; section: Section; revision: number; sampleInterval: number; onSampleInterval(value: number): void }) {
  const [tab, setTab] = useState('details');
  const container = 'state' in item ? item : null;
  const kind = section === 'images' ? 'image' : section === 'volumes' ? 'volume' : section === 'networks' ? 'network' : 'container';
  const id = 'id' in item ? item.id : item.name;
  const tabs = container ? ['details', 'logs', 'stats', 'files', 'processes', 'inspect'] as const : ['details', 'inspect'] as const;
  return <div class="detail-body"><div class="tabs" role="tablist">{tabs.map(value => <button role="tab" aria-selected={tab === value}
    onClick={() => setTab(value)}>{t(value)}</button>)}</div>
    {tab === 'details' && <ResourceSummary item={item} section={section} revision={revision}/>}
    {tab === 'logs' && container && <LogView key={id} id={id}/>}
    {tab === 'stats' && container && <StatsView key={id} id={id} state={container.state} interval={sampleInterval} onInterval={onSampleInterval}/>}
    {tab === 'files' && container && <ContainerFiles key={id} id={id}/>}
    {tab === 'processes' && <RemoteText key={`${id}-${revision}`} fn="containers.top" params={{ id }} label={t('processes')}/>}
    {tab === 'inspect' && <RemoteText key={`${id}-${revision}`} fn="objects.inspect" params={{ id, kind }} label={t('inspect')}/>}
  </div>;
}
