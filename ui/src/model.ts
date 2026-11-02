import type { Container, Resource, Section } from './types.ts';

export const sections: Section[] = ['containers', 'images', 'volumes', 'networks', 'system', 'connection'];
export function resourceKey(item: Resource): string { return 'id' in item ? item.id : item.name; }
export function resourceName(item: Resource): string {
  return 'tags' in item ? item.tags.join(', ') || item.id.replace('sha256:', '').slice(0, 12) : item.name;
}
export function visibleResources(items: Resource[], query: string, runningOnly: boolean): Resource[] {
  const needle = query.trim().toLocaleLowerCase();
  return items.filter(item => (!runningOnly || ('state' in item && item.state === 'running')) &&
    `${resourceName(item)} ${resourceKey(item)} ${'image' in item ? item.image : ''} ${'project' in item ? item.project ?? '' : ''}`
      .toLocaleLowerCase().includes(needle))
    .sort((a, b) => resourceName(a).localeCompare(resourceName(b), undefined, { numeric: true }));
}
export type ContainerRow = { type: 'group'; project: string | null; count: number; paths: string[] } | { type: 'item'; item: Container };
export function containerRows(items: Container[], collapsed: Set<string>): ContainerRow[] {
  const groups = new Map<string | null, Container[]>();
  for (const item of items) {
    const group = item.project || null;
    if (!groups.has(group)) groups.set(group, []);
    groups.get(group)!.push(item);
  }
  return [...groups].sort(([a], [b]) => (a ?? '\uffff').localeCompare(b ?? '\uffff')).flatMap(([project, containers]) => [
    { type: 'group', project, count: containers.length,
      paths: [...new Set(containers.flatMap(container => container.configFiles
        ? container.configFiles.split(',').map(path => path.trim()).filter(Boolean)
        : container.workingDirectory ? [container.workingDirectory] : []))] } as ContainerRow,
    ...collapsed.has(project ?? '') ? [] : containers.map(item => ({ type: 'item', item }) as ContainerRow),
  ]);
}
export function bytes(value: number | undefined): string {
  if (value === undefined || !Number.isFinite(value)) return '—';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const power = Math.min(4, Math.floor(Math.log2(Math.max(1, value)) / 10));
  return `${(value / 1024 ** power).toLocaleString(undefined, { maximumFractionDigits: power ? 1 : 0 })} ${units[power]}`;
}

// Keep form syntax deterministic; Docker receives argument arrays, never a shell command.
export function runParams(form: FormData): Record<string, unknown> {
  const value = (key: string) => String(form.get(key) ?? '').trim();
  const lines = (key: string) => value(key).split(/\r?\n/).map(line => line.trim()).filter(Boolean);
  const ports = lines('ports').map(line => {
    const match = /^(\d+):(\d+)(?:\/(tcp|udp|sctp))?$/.exec(line);
    if (!match || [match[1], match[2]].some(port => +port < 1 || +port > 65535)) throw new Error('invalidPorts');
    return { host: match[1], container: match[2], protocol: match[3] ?? 'tcp', address: '127.0.0.1' };
  });
  const environment = lines('environment');
  if (environment.some(line => !/^[^=\s]+=/.test(line))) throw new Error('invalidEnv');
  const volumes = lines('volumes').map(line => {
    const match = /^(.+?):(\/[^:]+?)(:ro)?$/.exec(line);
    if (!match) throw new Error('invalidMount');
    return { source: match[1], target: match[2], readOnly: !!match[3] };
  });
  let command: unknown = [];
  if (value('command')) {
    try { command = JSON.parse(value('command')); } catch { throw new Error('invalidCommand'); }
    if (!Array.isArray(command) || command.some(arg => typeof arg !== 'string')) throw new Error('invalidCommand');
  }
  return { image: value('image'), name: value('name') || undefined, ports, environment, volumes, command,
    restart: value('restart'), network: value('network') || undefined };
}
