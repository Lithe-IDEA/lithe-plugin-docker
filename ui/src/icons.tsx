import { Box, Boxes, Layers, HardDrive, Network, ChartPie, Settings2, Search,
  RefreshCw, Plus, Download, Play, Square, RotateCw, Pause, Trash2, ChevronRight,
  ChevronDown, CircleX, X, PackageOpen, Cable, WrapText, ArrowUp, Upload, Folder, File } from 'lucide-preact';

const icons = {
  containers: Box, images: Layers, volumes: HardDrive, networks: Network,
  system: ChartPie, connection: Settings2, search: Search, refresh: RefreshCw,
  run: Plus, pull: Download, start: Play, stop: Square, restart: RotateCw,
  pause: Pause, unpause: Play, kill: CircleX, remove: Trash2,
  collapsed: ChevronRight, expanded: ChevronDown, close: X, project: Boxes,
  empty: PackageOpen, endpoint: Cable, wrap: WrapText, up: ArrowUp, upload: Upload, folder: Folder, file: File,
};
export type IconName = keyof typeof icons;

export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  const Component = icons[name];
  return <Component size={size} strokeWidth={1.65} aria-hidden="true" focusable="false" class="icon"/>;
}
