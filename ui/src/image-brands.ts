// Fallbacks for exact, public image repositories whose registry has no logo
// API (or whose Hub logo is missing). Never match user container names.
const brands: Record<string, string> = {
  'docker.elastic.co/elasticsearch/elasticsearch': 'elasticsearch',
  'docker.elastic.co/kibana/kibana': 'kibana',
  'library/elasticsearch': 'elasticsearch', 'library/kibana': 'kibana',
  'minio/minio': 'minio', 'ghcr.io/open-webui/open-webui': 'open-webui',
  'ankane/pgvector': 'postgresql', 'pgvector/pgvector': 'postgresql',
};

export function bundledImageLogo(reference: string): string | null {
  let repository = reference.split('@')[0];
  // Tags occur after the final slash; registry ports are not tags.
  if (repository.lastIndexOf(':') > repository.lastIndexOf('/')) repository = repository.slice(0, repository.lastIndexOf(':'));
  repository = repository.replace(/^(?:docker\.io|index\.docker\.io|registry-1\.docker\.io)\//, '');
  if (!repository.includes('/')) repository = `library/${repository}`;
  const logo = brands[repository];
  return logo ? `../icons/brands/${logo}.svg` : null;
}
