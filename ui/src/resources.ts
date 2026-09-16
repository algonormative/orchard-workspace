export type ResourceKind = "channel" | "direct" | "broadcast" | "message" | "agent" | "task" | "file" | "url";
export type ResourceRef = { kind: ResourceKind; workspace_id: string; id?: string; store_id?: string; task_id?: string; root_id?: string; path?: string; revision?: string; url?: string };
export type Descriptor = { ref: ResourceRef; href: string; title: string; kind: ResourceKind };

/** Matches Rust RFC3986 percent encoding, including the five punctuation marks JS leaves raw. */
const segment = (value: string) => encodeURIComponent(value).replace(/[!'()*]/g, (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`);

const kind = (value: string): ResourceKind | undefined => ["channel", "direct", "broadcast", "message", "agent", "task", "file", "url"].includes(value) ? value as ResourceKind : undefined;

export function canonicalHref(ref: ResourceRef): string {
  const base = `/w/${segment(ref.workspace_id)}`;
  if (ref.kind === "channel") return `${base}/channels/${segment(ref.id || "")}`;
  if (ref.kind === "direct") return `${base}/direct/${segment(ref.id || "")}`;
  if (ref.kind === "broadcast") return `${base}/broadcast`;
  if (ref.kind === "message") return `${base}/messages/${segment(ref.id || "")}`;
  if (ref.kind === "agent") return `${base}/agents/${segment(ref.id || "")}`;
  if (ref.kind === "task") return `${base}/tasks/${segment(ref.store_id || "")}/${segment(ref.task_id || "")}`;
  if (ref.kind === "file") return `${base}/files/${segment(ref.root_id || "")}?path=${segment(ref.path || "")}${ref.revision ? `&revision=${segment(ref.revision)}` : ""}`;
  return `${base}/urls?url=${segment(ref.url || "")}`;
}

/** Parse only Orchard's canonical relative paths; never turn arbitrary hrefs into app state. */
export function parseHref(href: string, workspaceId: string): ResourceRef | undefined {
  try {
    const url = new URL(href, window.location.origin);
    if (url.origin !== window.location.origin || url.hash) return undefined;
    const parts = url.pathname.split("/").filter(Boolean).map(decodeURIComponent);
    if (parts[0] !== "w" || parts[1] !== workspaceId) return undefined;
    const route = parts[2]; const routeKind = kind(route === "channels" ? "channel" : route === "direct" ? "direct" : route === "messages" ? "message" : route === "agents" ? "agent" : route === "files" ? "file" : route === "urls" ? "url" : route === "broadcast" ? "broadcast" : route === "tasks" ? "task" : "");
    if (!routeKind) return undefined;
    let ref: ResourceRef | undefined;
    if (routeKind === "task") ref = parts.length === 5 && parts[3] && parts[4] ? { kind: routeKind, workspace_id: workspaceId, store_id: parts[3], task_id: parts[4] } : undefined;
    else if (routeKind === "file") {
      const path = url.searchParams.get("path") || ""; const revision = url.searchParams.get("revision") || undefined;
      ref = parts.length === 4 && parts[3] && path && (!revision || /^[0-9a-f]{40}$/.test(revision)) ? { kind: routeKind, workspace_id: workspaceId, root_id: parts[3], path, revision } : undefined;
    }
    else if (routeKind === "url") { const value = url.searchParams.get("url") || ""; if (parts.length === 3 && /^https?:\/\//.test(value)) ref = { kind: routeKind, workspace_id: workspaceId, url: value }; }
    else if (routeKind === "broadcast") ref = parts.length === 3 ? { kind: routeKind, workspace_id: workspaceId } : undefined;
    else ref = parts.length === 4 && parts[3] ? { kind: routeKind, workspace_id: workspaceId, id: parts[3] } : undefined;
    if (!ref || canonicalHref(ref) !== `${url.pathname}${url.search}`) return undefined;
    return ref;
  } catch { return undefined; }
}
