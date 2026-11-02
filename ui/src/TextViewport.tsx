import { useLayoutEffect, useRef } from 'preact/hooks';
import { Compartment, EditorState } from '@codemirror/state';
import { Decoration, EditorView, ViewPlugin } from '@codemirror/view';
import { search, highlightSelectionMatches, searchKeymap, SearchQuery, setSearchQuery, findNext, findPrevious } from '@codemirror/search';
import { keymap } from '@codemirror/view';
import { webLinks, openWebLink } from './links.tsx';
import { errorMessage, t } from './i18n.ts';
import { useState } from 'preact/hooks';
import type { DecorationSet } from '@codemirror/view';
import { Icon } from './icons.tsx';
import { json, jsonLanguage } from '@codemirror/lang-json';
import { HighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { highlightTree, tagHighlighter, tags } from '@lezer/highlight';

const jsonStyles = [
  { tag: tags.propertyName, class: 'json-key' }, { tag: tags.string, class: 'json-string' },
  { tag: tags.number, class: 'json-number' }, { tag: [tags.bool, tags.null], class: 'json-literal' },
  { tag: tags.punctuation, class: 'json-punctuation' },
];
const jsonTokens = tagHighlighter(jsonStyles);

function logPresentation(prefixes: number[]) {
  return ViewPlugin.fromClass(class {
    decorations: DecorationSet;
    constructor(view: EditorView) { this.decorations = this.build(view); }
    update(update: { docChanged: boolean; viewportChanged: boolean; view: EditorView }) {
      if (update.docChanged || update.viewportChanged) this.decorations = this.build(update.view);
    }
    build(view: EditorView) {
      const ranges = [];
      const seen = new Set<number>();
      for (const visible of view.visibleRanges) {
        for (let number = view.state.doc.lineAt(visible.from).number; number <= view.state.doc.lineAt(visible.to).number; number++) {
          if (seen.has(number)) continue; seen.add(number);
          const line = view.state.doc.line(number);
          const prefix = Math.min(prefixes[number - 1] ?? 0, line.length);
          if (prefix) ranges.push(Decoration.mark({ class: 'log-timestamp' }).range(line.from, line.from + prefix));
          const body = line.text.slice(prefix);
          if (!/^\s*[\[{]/.test(body)) continue;
          try { JSON.parse(body); } catch { continue; }
          highlightTree(jsonLanguage.parser.parse(body), jsonTokens, (from, to, classes) => {
            ranges.push(Decoration.mark({ class: classes }).range(line.from + prefix + from, line.from + prefix + to));
          });
        }
      }
      return Decoration.set(ranges, true);
    }
  }, { decorations: plugin => plugin.decorations });
}

const links = ViewPlugin.fromClass(class {
  decorations: DecorationSet;
  constructor(view: EditorView) { this.decorations = this.build(view); }
  update(update: { docChanged: boolean; viewportChanged: boolean; view: EditorView }) {
    if (update.docChanged || update.viewportChanged) this.decorations = this.build(update.view);
  }
  build(view: EditorView) {
    const ranges: { from: number; to: number }[] = [];
    for (const range of view.visibleRanges) {
      const from = view.state.doc.lineAt(range.from).from;
      const to = view.state.doc.lineAt(range.to).to;
      const previous = ranges.at(-1);
      if (previous && from <= previous.to) previous.to = Math.max(previous.to, to);
      else ranges.push({ from, to });
    }
    return Decoration.set(ranges.flatMap(range => webLinks(view.state.doc.sliceString(range.from, range.to))
      .map(link => Decoration.mark({ tagName: 'a', class: 'web-link', attributes: { href: link.url, tabindex: '0' } })
        .range(range.from + link.from, range.from + link.to))));
  }
}, { decorations: plugin => plugin.decorations });

// CodeMirror owns visible line layout, selection and native two-axis scrolling.
// It also virtualizes a single very long line; the page does not measure or
// split it into thousands of mounted elements when wrapping or resizing.
export function TextViewport({ value, label, wrap, follow = false, timestampLengths = [], showSearch = false, language = 'text' }:
  { value: string; label: string; wrap: boolean; follow?: boolean; timestampLengths?: number[]; showSearch?: boolean; language?: 'text' | 'json' | 'logs' }) {
  const parent = useRef<HTMLDivElement>(null);
  const [linkError, setLinkError] = useState('');
  const [query, setQuery] = useState('');
  const view = useRef<EditorView | null>(null);
  const wrapping = useRef(new Compartment());
  const accessibility = useRef(new Compartment());
  const presentation = useRef(new Compartment());
  useLayoutEffect(() => {
    const editor = new EditorView({ parent: parent.current!, state: EditorState.create({
      doc: value,
      extensions: [EditorState.readOnly.of(true), links, search(), highlightSelectionMatches(), keymap.of(searchKeymap),
        presentation.current.of([]),
        EditorState.phrases.of({ Find: t('find'), next: t('nextMatch'), previous: t('previousMatch'),
          all: t('allMatches'), 'match case': t('matchCase'), regexp: t('regexp'), 'by word': t('wholeWord'), close: t('close'),
          'current match': t('currentMatch'), 'on line': t('onLine') }),
        EditorView.domEventHandlers({
          click(event) {
            const anchor = (event.target as Element).closest('a.web-link');
            if (!anchor) return false;
            event.preventDefault(); setLinkError('');
            void openWebLink(anchor.getAttribute('href')!).catch(error => setLinkError(errorMessage(error)));
            return true;
          },
          keydown(event) {
            if (event.key !== 'Enter' || !(event.target as Element).matches('a.web-link')) return false;
            event.preventDefault(); void openWebLink((event.target as Element).getAttribute('href')!).catch(error => setLinkError(errorMessage(error))); return true;
          },
        }),
        wrapping.current.of(wrap ? EditorView.lineWrapping : []),
        accessibility.current.of(EditorView.contentAttributes.of({ 'aria-label': label })),
        EditorView.theme({
          '&': { height: '100%', backgroundColor: 'var(--background)', color: 'var(--foreground)', fontSize: '12px' },
          '&.cm-focused': { outline: 'none' },
          '.cm-scroller': { overflow: 'scroll', fontFamily: 'var(--code-font)', lineHeight: '22px' },
          '.cm-content': { padding: '8px 0 12px' },
          '.cm-line': { padding: '0 12px' },
          '.cm-cursor': { display: 'none' },
          '.cm-selectionBackground, &.cm-focused .cm-selectionBackground': { backgroundColor: 'var(--selection)' },
        }),
      ],
    }) });
    view.current = editor;
    return () => { view.current = null; editor.destroy(); };
  }, []);
  useLayoutEffect(() => {
    const editor = view.current!;
    const previous = editor.state.doc.toString();
    const changes = previous === value ? undefined : value.startsWith(previous)
      ? { from: previous.length, insert: value.slice(previous.length) }
      : { from: 0, to: previous.length, insert: value };
    editor.dispatch({ changes, effects: [
      presentation.current.reconfigure(language === 'json' ? [json(), syntaxHighlighting(HighlightStyle.define(jsonStyles))]
        : language === 'logs' ? logPresentation(timestampLengths) : []),
      wrapping.current.reconfigure(wrap ? EditorView.lineWrapping : []),
      accessibility.current.reconfigure(EditorView.contentAttributes.of({ 'aria-label': label })),
      ...(follow ? [EditorView.scrollIntoView(value.length, { y: 'end' })] : []),
    ] });
  }, [value, wrap, follow, label, language, timestampLengths.join(',')]);
  useLayoutEffect(() => { view.current?.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: query })) }); }, [query]);
  return <>{showSearch && <div class="text-search"><div class="search-field"><Icon name="search" size={14}/><input type="search" placeholder={t('searchLogs')} aria-label={t('searchLogs')} value={query} onInput={event => setQuery(event.currentTarget.value)} onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); if (view.current) (event.shiftKey ? findPrevious : findNext)(view.current); } }}/></div>
    <button class="icon-button" title={t('previousMatch')} aria-label={t('previousMatch')} disabled={!query} onClick={() => { if (view.current) findPrevious(view.current); }}><Icon name="up" size={14}/></button>
    <button class="icon-button" title={t('nextMatch')} aria-label={t('nextMatch')} disabled={!query} onClick={() => { if (view.current) findNext(view.current); }}><Icon name="expanded" size={14}/></button></div>}
    {linkError && <div class="error" role="alert">{linkError}</div>}<div class={`text-viewport ${wrap ? 'is-wrapped' : ''}`} ref={parent}/></>;
}
