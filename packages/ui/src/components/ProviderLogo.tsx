export function ProviderLogo({ svg, name }: { svg?: string | null; name: string }) {
  if (!svg) {
    return <span className="providerLogoFallback" aria-hidden="true">{name.slice(0, 1).toUpperCase()}</span>;
  }
  return <img className="providerLogo" src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`} alt="" aria-hidden="true" />;
}
