import type { RefObject } from 'preact';
import { useLayoutEffect, useRef } from 'preact/hooks';
import { startDocumentResizeSession } from './document-resize-session.ts';
import type { DocumentResizeSession } from './document-resize-session.ts';

export function ColumnHandle({ list, column, label, onCommit }:
  { list: RefObject<HTMLElement | null>; column: 0 | 1; label: string; onCommit(width: number): void }) {
  const session = useRef<DocumentResizeSession | null>(null);
  useLayoutEffect(() => () => session.current?.dispose({ commit: false }), []);
  const read = () => list.current!.querySelectorAll('.table-header>span')[column].getBoundingClientRect().width;
  const apply = (width: number) => {
    list.current!.style.setProperty(column === 0 ? '--name-width' : '--image-width', `${width}px`);
    list.current!.style.setProperty(column === 0 ? '--name-min' : '--image-min', `${width}px`);
  };
  return <span class="column-handle" role="separator" aria-orientation="vertical" tabIndex={0}
    aria-label={label} title={label} onPointerDown={event => {
      if (event.button !== 0 || !event.isPrimary) return;
      event.preventDefault(); session.current?.dispose();
      session.current = startDocumentResizeSession({
        startX: event.clientX, startWidth: read(), pointerID: event.pointerId,
        clampWidth: width => Math.max(90, Math.min(1600, width)), applyWidth: apply,
        commitWidth: width => { apply(width); onCommit(width); },
      });
    }} onKeyDown={event => {
      if (!['ArrowLeft', 'ArrowRight', 'Home'].includes(event.key)) return;
      event.preventDefault();
      const width = event.key === 'Home' ? 90 : Math.min(1600, Math.max(90, read() + (event.key === 'ArrowLeft' ? -10 : 10)));
      apply(width); onCommit(width);
    }}/>;
}
