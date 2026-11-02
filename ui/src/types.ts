export type Section = 'containers' | 'images' | 'volumes' | 'networks' | 'system' | 'connection';
export interface Endpoint { socket: string; context: string | null; source: string }
export interface Engine {
  reachable: boolean; endpoint?: Endpoint;
  error?: { code: string; message: string };
  version?: { Version: string; ApiVersion: string; Os: string; Arch: string; KernelVersion: string };
  platform?: string;
  info?: { Name: string; OperatingSystem: string; NCPU: number; MemTotal: number;
    ContainersRunning: number; ContainersStopped: number; Images: number; Driver: string };
}
export interface Contexts {
  contexts: { name: string; host: string; description: string; available: boolean }[];
  current: string | null; choice: { context?: string; socket?: string }; endpoint: Endpoint | null;
}
export interface Container {
  id: string; name: string; image: string; imageID: string; command: string;
  state: string; status: string; project: string | null; service: string | null;
  configFiles?: string | null; workingDirectory?: string | null;
  ports: { private: number; public: number | null; protocol: string; address: string }[];
  networks: string[]; mounts: { type: string; name: string; source: string; destination: string; readWrite: boolean }[];
}
export interface Image { id: string; tags: string[]; size: number; created: number; containers: number; dangling: boolean }
export interface Volume { name: string; driver: string; mountpoint: string; project: string | null; containers: string[] }
export interface Network { id: string; name: string; driver: string; scope: string; subnets: string[]; builtin: boolean; containers: string[] }
export type Resource = Container | Image | Volume | Network;
export interface LogLine { stream: string; time: string; text: string }
export interface Logs { lines: LogLine[]; cursor: string | null; ended: boolean; truncated: boolean }
export interface Stats {
  cpuPercent: number; memoryUsed: number; memoryLimit: number; networkReceived: number;
  networkSent: number; blockRead: number; blockWritten: number; processes: number;
}
export interface UsageEntry { count: number; active: number; size: number; reclaimable: number }
export type Usage = Record<'images' | 'containers' | 'volumes' | 'buildCache', UsageEntry>;
export interface Events { events: { type: string; action: string; id: string }[]; cursor: string | null }
