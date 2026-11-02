import type { ComponentChildren } from 'preact';
import { useId, useLayoutEffect, useRef, useState } from 'preact/hooks';

// One fixed-height viewport shared by resources, logs, process rows and JSON.
// Selection lives in the caller and survives unmounting an offscreen row.
export function VirtualList<T>({ items, height = 32, render, itemKey, selected, onSelect, follow = false, label, header, outerScroll = false }:
  { items: T[]; height?: number; render(item: T, index: number): ComponentChildren;
    itemKey(item: T, index: number): string; selected?: string; onSelect?: (item: T) => void;
    follow?: boolean; label: string; header?: ComponentChildren; outerScroll?: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  const identity = useId();
  const [viewport, setViewport] = useState({ top: 0, height: 400 });
  useLayoutEffect(() => {
    const element = ref.current!;
    const scroll = outerScroll ? element.closest<HTMLElement>('.resource-summary')! : element;
    let frame = 0;
    const measure = () => {
      frame = 0;
      if (!outerScroll) setViewport({ top: element.scrollTop, height: element.clientHeight });
      else {
        const rect = element.getBoundingClientRect(); const bounds = scroll.getBoundingClientRect();
        const top = Math.max(0, bounds.top - rect.top);
        setViewport({ top, height: Math.max(0, Math.min(rect.bottom, bounds.bottom) - Math.max(rect.top, bounds.top)) });
      }
    };
    const schedule = () => { if (!frame) frame = requestAnimationFrame(measure); };
    const observer = new ResizeObserver(schedule);
    observer.observe(element);
    if (outerScroll) { observer.observe(scroll); scroll.addEventListener('scroll', schedule, { passive: true }); }
    measure();
    return () => { observer.disconnect(); scroll.removeEventListener('scroll', schedule); cancelAnimationFrame(frame); };
  }, [outerScroll]);
  useLayoutEffect(() => {
    const element = ref.current!;
    if (outerScroll) return;
    if (follow) element.scrollTop = element.scrollHeight;
    else if (element.scrollTop > items.length * height - element.clientHeight) {
      element.scrollTop = Math.max(0, items.length * height - element.clientHeight);
    }
    setViewport({ top: element.scrollTop, height: element.clientHeight });
  }, [items.length, follow, height, outerScroll]);
  const start = Math.max(0, Math.floor(viewport.top / height) - 5);
  const end = Math.min(items.length, Math.ceil((viewport.top + viewport.height) / height) + 5);
  return <div class={`virtual-wrap ${outerScroll ? 'outer-scroll' : ''}`}>{header}<div class="virtual" ref={ref} tabIndex={0}
    role={onSelect ? 'listbox' : 'region'} aria-label={label} aria-activedescendant={selected ? `${identity}-${selected}` : undefined}
    onScroll={event => setViewport({ top: event.currentTarget.scrollTop, height: event.currentTarget.clientHeight })}
    onKeyDown={event => {
      if (!onSelect || !items.length || !['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
      event.preventDefault();
      const current = items.findIndex((item, index) => itemKey(item, index) === selected);
      const index = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1
        : Math.min(items.length - 1, Math.max(0, current + (event.key === 'ArrowDown' ? 1 : -1)));
      onSelect(items[index]);
      const element = event.currentTarget;
      if (index * height < element.scrollTop) element.scrollTop = index * height;
      else if ((index + 1) * height > element.scrollTop + element.clientHeight) element.scrollTop = (index + 1) * height - element.clientHeight;
    }}>
    {/* Preact 11 forwards CSS values verbatim; lengths need explicit units. */}
    <div class="virtual-space" style={{ height: `${items.length * height}px` }}>
      {items.slice(start, end).map((item, local) => {
        const index = local + start; const key = itemKey(item, index);
        return <div key={key} id={`${identity}-${key}`} data-index={index} class={`virtual-row ${key === selected ? 'selected' : ''}`}
          role={onSelect ? 'option' : undefined} aria-selected={onSelect ? key === selected : undefined}
          aria-posinset={onSelect ? index + 1 : undefined} aria-setsize={onSelect ? items.length : undefined}
          style={{ top: `${index * height}px`, height: `${height}px` }} onClick={onSelect ? () => onSelect(item) : undefined}>
          {render(item, index)}
        </div>;
      })}
    </div>
  </div></div>;
}
