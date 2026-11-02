import type { ComponentChildren } from 'preact';
import { useEffect, useState } from 'preact/hooks';
import { invoke, cancelled } from './bridge.ts';
import { LinkedText, WebLink } from './links.tsx';
import { VirtualList } from './VirtualList.tsx';
import { bytes, resourceKey, resourceName } from './model.ts';
import { errorMessage, t } from './i18n.ts';
import type { Resource, Section } from './types.ts';

type RecordValue = Record<string, unknown>;
type Row = [string, ComponentChildren];
const text = (value: unknown) => typeof value === 'string' || typeof value === 'number' ? String(value) : '—';

function InfoCard({ rows }: { rows: Row[] }) {
  return <dl class="info-card">{rows.map(([name, value]) => <div><dt>{name}</dt><dd>{value}</dd></div>)}</dl>;
}

function DataTable({ title, headings, rows }: { title: string; headings: string[]; rows: ComponentChildren[][] }) {
  return <section class="detail-section"><h3>{title}<span class="count">{rows.length}</span></h3>
    {rows.length ? <div class="data-table" style={{ height: `${rows.length * 30 + 30}px`, '--table-columns': `repeat(${headings.length}, minmax(0, 1fr))` }}>
      <VirtualList items={rows} height={30} itemKey={(_, index) => String(index)} label={title} outerScroll
        header={<div class="data-row data-header">{headings.map(label => <span>{label}</span>)}</div>}
        render={row => <div class="data-row">{row.map(value => <span>{value}</span>)}</div>}/>
    </div> : <div class="card-empty">{t('none')}</div>}
  </section>;
}

export function ResourceSummary({ item, section, revision }: { item: Resource; section: Section; revision: number }) {
  const [raw, setRaw] = useState<RecordValue | null>(null);
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  const id = resourceKey(item);
  const kind = section === 'images' ? 'image' : section === 'volumes' ? 'volume' : section === 'networks' ? 'network' : 'container';
  useEffect(() => {
    const controller = new AbortController(); setRaw(null); setError('');
    void invoke<RecordValue>('objects.inspect', { kind, id }, controller.signal).result.then(value => { if (!controller.signal.aborted) setRaw(value); })
      .catch(failure => { if (!controller.signal.aborted && !cancelled(failure)) setError(errorMessage(failure)); });
    return () => controller.abort();
  }, [kind, id, revision, attempt]);
  const container = 'state' in item ? item : null;
  const config = raw?.Config as RecordValue | undefined;
  const labels = (container || section === 'images' ? config?.Labels : raw?.Labels) as Record<string, string> | undefined;
  const options = raw?.Options as Record<string, string> | undefined;
  const ipam = raw?.IPAM as { Config?: { Subnet?: string; Gateway?: string }[] } | undefined;
  const networks = (raw?.NetworkSettings as { Networks?: Record<string, { IPAddress?: string; Gateway?: string }> } | undefined)?.Networks;
  const basic: Row[] = [[t('name'), resourceName(item)], [t('id'), id]];
  if (container) basic.push([t('image'), container.image], [t('state'), <span class={`state-label ${container.state}`}><span class={`dot ${container.state}`}/>{container.status}</span>]);
  if ('size' in item) basic.push([t('size'), bytes(item.size)]);
  if (raw?.Created) basic.push([t('created'), text(raw.Created)]);
  return <div class="scroll-content resource-summary">
    {error && <div class="error">{error}<button onClick={() => setAttempt(value => value + 1)}>{t('retry')}</button></div>}
    <InfoCard rows={basic}/>
    {container && <>
      <InfoCard rows={[[t('command'), <LinkedText value={container.command}/>], [t('project'), container.project ?? t('none')], [t('service'), container.service ?? t('none')]]}/>
      <DataTable title={t('ports')} headings={[t('hostPort'), t('containerPort'), t('protocol')]}
        rows={container.ports.map(port => {
          const host = !port.address || ['0.0.0.0', '::'].includes(port.address) ? '127.0.0.1' : port.address;
          const address = host.includes(':') ? `[${host}]` : host;
          return [port.public && port.protocol === 'tcp' ? <WebLink url={`http://${address}:${port.public}`}>{`${port.public}`}</WebLink> : port.public ?? '—', port.private, port.protocol.toUpperCase()];
        })}/>
      <DataTable title={t('networks')} headings={[t('name'), t('ipAddress'), t('gateway')]}
        rows={container.networks.map(name => [name, networks?.[name]?.IPAddress || '—', networks?.[name]?.Gateway || '—'])}/>
      <DataTable title={t('mounts')} headings={[t('source'), t('destination'), t('mode')]}
        rows={container.mounts.map(mount => [<LinkedText value={mount.name || mount.source}/>, mount.destination, mount.readWrite ? 'rw' : 'ro'])}/>
    </>}
    {!container && 'driver' in item && <InfoCard rows={[[t('driver'), item.driver], ...('scope' in item ? [[t('scope'), item.scope] as Row] : []),
      ...('mountpoint' in item ? [[t('destination'), item.mountpoint] as Row] : [])]}/>}
    {section === 'networks' && <DataTable title={t('networkConfiguration')} headings={[t('subnet'), t('gateway')]}
      rows={(ipam?.Config ?? []).map(value => [value.Subnet ?? '—', value.Gateway ?? '—'])}/>}
    {options && <DataTable title={t('options')} headings={[t('key'), t('value')]} rows={Object.entries(options).map(([key, value]) => [key, <LinkedText value={text(value)}/>])}/>}
    {labels && <DataTable title={t('labels')} headings={[t('key'), t('value')]} rows={Object.entries(labels).sort(([a], [b]) => a.localeCompare(b)).map(([key, value]) => [key, <LinkedText value={text(value)}/>])}/>}
    {!raw && !error && <div class="detail-loading">{t('loading')}</div>}
  </div>;
}
