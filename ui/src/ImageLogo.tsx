import { useEffect, useState } from 'preact/hooks';
import { invoke } from './bridge.ts';
import { Icon } from './icons.tsx';
import type { IconName } from './icons.tsx';
import { bundledImageLogo } from './image-brands.ts';

const pending = new Map<string, Promise<string | null>>();

// The backend resolves public Docker Hub repository logos and owns the cache.
// The page never opens a network connection or guesses logos by container name.
export function ImageLogo({ image, fallback }: { image: string; fallback: IconName }) {
  const fallbackLogo = bundledImageLogo(image);
  const [logo, setLogo] = useState<string | null>(fallbackLogo);
  useEffect(() => {
    let active = true;
    setLogo(fallbackLogo);
    if (!image) return;
    let request = pending.get(image);
    if (!request) {
      request = invoke<{ data: string | null }>('images.logo', { image }).result.then(answer => answer.data);
      pending.set(image, request);
      if (pending.size > 256) pending.delete(pending.keys().next().value!);
    }
    void request.then(value => { if (active) setLogo(value ?? fallbackLogo); }).catch(() => { pending.delete(image); });
    return () => { active = false; };
  }, [image]);
  return logo ? <img class="resource-logo" src={logo} alt="" width="20" height="20" onError={() => setLogo(logo === fallbackLogo ? null : fallbackLogo)}/>
    : <span class="resource-logo"><Icon name={fallback} size={15}/></span>;
}
