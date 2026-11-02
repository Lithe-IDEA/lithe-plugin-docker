import type { RefObject } from 'preact';
import { useLayoutEffect, useRef } from 'preact/hooks';
import { startDocumentResizeSession } from './document-resize-session.ts';
import type { DocumentResizeSession } from './document-resize-session.ts';

interface Props {
  pane: RefObject<HTMLElement | null>;
  container: RefObject<HTMLElement | null>;
  kind: 'sidebar' | 'details';
  label: string;
  onCommit(size: number, axis: 'x' | 'y'): void;
}

// Layout is local DOM state during a drag. Only the final size reaches App
// and the host's existing ui.setState persistence. Native Swift splitters
// cannot operate inside the plugin's isolated WebKit page.
export function ResizeHandle({ pane, container, kind, label, onCommit }: Props) {
  const handle = useRef<HTMLDivElement>(null);
  const session = useRef<DocumentResizeSession | null>(null);
  const geometry = () => {
    const vertical = kind === 'details' && getComputedStyle(container.current!).flexDirection === 'column';
    const axis = vertical ? 'y' : 'x';
    const extent = vertical ? container.current!.clientHeight : container.current!.clientWidth;
    const minimum = kind === 'sidebar' ? 48 : vertical ? 100 : 240;
    const reserved = kind === 'sidebar' ? 280 : vertical ? 100 : 205;
    const maximum = Math.max(minimum, Math.min(kind === 'sidebar' ? 360 : Infinity, extent - reserved));
    const size = vertical ? pane.current!.clientHeight : pane.current!.clientWidth;
    return { axis, minimum, maximum, size } as const;
  };
  const apply = (size: number, axis: 'x' | 'y') => {
    pane.current!.style.setProperty(axis === 'x' ? '--pane-width' : '--pane-height', `${size}px`);
    handle.current!.setAttribute('aria-valuenow', String(Math.round(size)));
  };
  useLayoutEffect(() => {
    const observer = new ResizeObserver(() => {
      const { axis, minimum, maximum, size } = geometry();
      handle.current!.setAttribute('aria-orientation', axis === 'x' ? 'vertical' : 'horizontal');
      handle.current!.setAttribute('aria-valuemin', String(minimum));
      handle.current!.setAttribute('aria-valuemax', String(maximum));
      handle.current!.setAttribute('aria-valuenow', String(Math.round(size)));
      if (kind === 'sidebar') pane.current!.dataset.compact = String(size < 130);
    });
    observer.observe(pane.current!);
    observer.observe(container.current!);
    return () => { observer.disconnect(); session.current?.dispose({ commit: false }); };
  }, []);
  return <div ref={handle} class={`resize-handle resize-${kind}`} role="separator" tabIndex={0}
    aria-label={label} title={label} onPointerDown={event => {
      if (event.button !== 0 || !event.isPrimary) return;
      event.preventDefault();
      session.current?.dispose();
      const { axis, minimum, maximum, size } = geometry();
      session.current = startDocumentResizeSession({
        startX: axis === 'x' ? event.clientX : event.clientY, startWidth: size, axis,
        direction: kind === 'sidebar' ? 1 : -1,
        cursor: axis === 'x' ? 'ew-resize' : 'ns-resize', pointerID: event.pointerId,
        clampWidth: value => Math.min(maximum, Math.max(minimum, value)),
        applyWidth: value => apply(value, axis),
        commitWidth: value => { apply(value, axis); onCommit(value, axis); },
      });
    }} onKeyDown={event => {
      const { axis, minimum, maximum, size } = geometry();
      const negative = axis === 'x' ? 'ArrowLeft' : 'ArrowUp';
      const positive = axis === 'x' ? 'ArrowRight' : 'ArrowDown';
      if (!['Home', 'End', negative, positive].includes(event.key)) return;
      event.preventDefault();
      const delta = (event.key === positive ? 1 : -1) * (kind === 'sidebar' ? 1 : -1) * (event.shiftKey ? 40 : 10);
      const next = event.key === 'Home' ? minimum : event.key === 'End' ? maximum : Math.min(maximum, Math.max(minimum, size + delta));
      apply(next, axis); onCommit(next, axis);
    }}/>;
}
