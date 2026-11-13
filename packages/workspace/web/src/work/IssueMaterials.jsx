import { useEffect, useState } from "react";
import { Button } from "@centaeris/ui/button";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";

export function IssueMaterials({ workspaceId, issue, onChanged }) {
  const { t } = useTranslation();
  const endpoint = `/api/workspaces/${encodeURIComponent(workspaceId)}/issues/${encodeURIComponent(issue.id)}/materials`;
  const [data, setData] = useState(null);
  const [choosing, setChoosing] = useState(false);
  const [folderPath, setFolderPath] = useState([]);
  const folderId = folderPath.at(-1)?.id || "";
  const [sourceId, setSourceId] = useState("");
  const [sources, setSources] = useState([]);
  const [objects, setObjects] = useState([]);
  const [pending, setPending] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Explicit reload refreshes material authorization and concurrency version.
  useEffect(() => {
    const controller = new AbortController();
    apiJson(endpoint, { signal: controller.signal }).then(result => { if (!controller.signal.aborted) setData(result); }).catch(failure => { if (!controller.signal.aborted) setError(failure.message); });
    return () => controller.abort();
  }, [endpoint, issue.version, revision]);
  useEffect(() => {
    if (!choosing) return;
    const controller = new AbortController();
    apiJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/sources`, { signal: controller.signal }).then(result => { if (!controller.signal.aborted) setSources(result.sources); }).catch(failure => { if (!controller.signal.aborted) setError(failure.message); });
    return () => controller.abort();
  }, [choosing, workspaceId]);
  useEffect(() => {
    if (!choosing) return;
    const controller = new AbortController();
    setObjects([]); setLoading(true);
    const path = sourceId ? `/api/workspaces/${encodeURIComponent(workspaceId)}/sources/${encodeURIComponent(sourceId)}/objects` : `/api/library${folderId ? `?parentFolderId=${encodeURIComponent(folderId)}` : ""}`;
    apiJson(path, { signal: controller.signal }).then(result => { if (!controller.signal.aborted) setObjects(result.objects); }).catch(failure => { if (!controller.signal.aborted) setError(failure.message); }).finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [choosing, workspaceId, sourceId, folderId]);
  async function mutate(path, method, body) {
    setPending(true); setError("");
    try { const result = await apiJson(path, jsonOptions(method, { version: data.version, ...body })); setData(result); onChanged(); }
    catch (failure) { setError(["issue_material_unavailable", "issue_material_version_changed"].includes(failure.message) ? t("issues.materialsChanged") : failure.status === 409 ? t("issues.conflict") : failure.message); }
    finally { setPending(false); }
  }
  async function upload(files) {
    if (!files.length) return;
    setPending(true); setError("");
    let changed = false;
    try {
      const body = new FormData();
      for (const file of files) body.append("files", file);
      const result = await apiJson("/api/library", { method: "POST", body });
      let version = data.version;
      for (const object of result.objects) {
        const attached = await apiJson(endpoint, jsonOptions("POST", { version, assetKind: "userLibraryObject", assetId: object.id }));
        version = attached.version; setData(attached); changed = true;
      }
    } catch (failure) { setError(["issue_material_unavailable", "issue_material_version_changed"].includes(failure.message) ? t("issues.materialsChanged") : failure.status === 409 ? t("issues.conflict") : failure.message); }
    finally { setPending(false); if (changed) onChanged(); }
  }
  return <section className="issueMaterials" aria-busy={pending}>
    <header><h2>{t("issues.materials")}</h2>{!issue.archivedAt ? <Button variant="ghost" disabled={!data || pending} onClick={() => setChoosing(value => !value)} aria-expanded={choosing}>{t("issues.addMaterials")}</Button> : null}</header>
    {error ? <div role="alert" className="issueExecutionError">{error}<Button variant="ghost" disabled={pending} onClick={() => { setError(""); setRevision(value => value + 1); onChanged(); }}>{t("issues.reload")}</Button></div> : null}
    {data ? <ul className="issueMaterialList">{data.materials.map(material => <li key={material.id}><span>{material.available ? material.displayName : t("issues.materialUnavailable")}</span>{!issue.archivedAt ? <Button variant="ghost" disabled={pending} aria-label={t("issues.removeMaterial", { name: material.available ? material.displayName : t("issues.materialUnavailable") })} onClick={() => void mutate(`${endpoint}/${encodeURIComponent(material.id)}`, "DELETE", {})}>{t("issues.remove")}</Button> : null}</li>)}</ul> : null}
    {choosing && !issue.archivedAt ? <div className="issueMaterialChooser">
      <label>{t("issues.materialLocation")}<select value={sourceId} disabled={pending} onChange={event => setSourceId(event.target.value)}><option value="">{t("space.materials")}</option>{sources.map(source => <option value={source.id} key={source.id}>{source.name}</option>)}</select></label>
      {!sourceId ? <><label className="issueUpload">{t("issues.uploadMaterials")}<input type="file" multiple disabled={pending} onChange={event => { void upload(Array.from(event.target.files)); event.target.value = ""; }} /></label>{folderId ? <Button variant="ghost" disabled={pending} onClick={() => setFolderPath(path => path.slice(0, -1))}>{t("issues.parentFolder")}</Button> : null}</> : null}
      {loading ? <p role="status">{t("agentChat.loading")}</p> : <ul className="issueMaterialList">{objects.filter(object => object.status === "ready" && (sourceId ? object.objectType === "file" : ["folder", "file", "image", "note", "savedArtifact"].includes(object.objectKind))).map(object => <li key={object.id}><span>{object.displayPath || object.displayName}</span>{!sourceId && object.objectKind === "folder" ? <Button variant="ghost" disabled={pending} onClick={() => setFolderPath(path => [...path, object])}>{t("issues.openFolder")}</Button> : <Button variant="ghost" disabled={pending || !data || data.materials.some(material => material.assetKind === (sourceId ? "sourceObject" : "userLibraryObject") && material.assetId === object.id)} onClick={() => void mutate(endpoint, "POST", { assetKind: sourceId ? "sourceObject" : "userLibraryObject", assetId: object.id })}>{t("issues.attach")}</Button>}</li>)}</ul>}
    </div> : null}
  </section>;
}
