import { useEffect, useRef, useState } from 'preact/hooks';
import { cancelled, invoke } from './bridge.ts';
import { errorMessage, t } from './i18n.ts';
import { Icon } from './icons.tsx';

export function ContainerTerminal({ id, name, running }: { id: string; name: string; running: boolean }) {
  const [shell, setShell] = useState('/bin/sh');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const pending = useRef<AbortController | undefined>(undefined);
  useEffect(() => () => pending.current?.abort(), []);
  async function perform(dispose = false) {
    if (pending.current) return;
    const controller = new AbortController(); pending.current = controller;
    setBusy(true); setError('');
    try {
      await invoke(dispose ? 'containers.terminalDispose' : 'containers.terminal', {
        id, shell, title: t('containerTerminalTitle', { name: [...name].slice(0, 24).join('') }),
      }, controller.signal).result;
    } catch (failure) {
      if (!controller.signal.aborted && !cancelled(failure)) setError(errorMessage(failure));
    } finally {
      pending.current = undefined;
      setBusy(false);
    }
  }
  return <><div class="container-terminal"><label>{t('containerShell')} <select value={shell} disabled={busy}
      aria-label={t('containerShell')} onChange={event => setShell(event.currentTarget.value)}>
      {['/bin/sh', '/bin/bash', '/bin/ash'].map(value => <option value={value}>{value}</option>)}
    </select></label>
      <button disabled={busy || !running} title={t(running ? 'terminalHelp' : 'docker.terminalStopped')}
        onClick={() => void perform()}><Icon name="terminal"/>{t('openTerminal')}</button>
      {busy ? <button onClick={() => pending.current?.abort()}>{t('cancel')}</button>
        : <button class="icon-button" aria-label={t('disposeTerminal')} title={t('disposeTerminal')}
          onClick={() => void perform(true)}><Icon name="close"/></button>}
    </div>
    {error && <div class="error terminal-error" role="alert">{error}</div>}
  </>;
}
