import { apiUrl } from "../api";
import { i18n } from "../i18n";
import { officeFileType } from "./officeFormats.mjs";

export function officePreviewUrl(kind, id) {
  const previewUrl = apiUrl(`/api/office-preview/${encodeURIComponent(kind)}/${encodeURIComponent(id)}?lang=${i18n.language === "en" ? "en" : "zh-CN"}`);
  return previewUrl;
}

export function attachmentDownloadUrl(link) {
  if (link.assetKind === "userLibraryObject") return apiUrl(`/api/library/${link.asset.id}/download`);
  if (link.assetKind === "sourceObject") return apiUrl(`/api/source-objects/${link.asset.id}/download`);
  if (link.assetKind === "artifact") return apiUrl(`/api/artifacts/${link.asset.id}/download`);
  throw new Error(`unsupported attachment assetKind: ${link.assetKind}`);
}

export function attachmentPreviewUrl(link) {
  if (officeFileType(link.displayName || link.asset?.displayName || "")) return officePreviewUrl(link.assetKind, link.asset.id);
  return link.assetKind === "userLibraryObject"
    ? apiUrl(`/api/library/${link.asset.id}/preview`)
    : attachmentDownloadUrl(link);
}

export function attachmentIsImage(link) {
  return (link.contentType || link.asset?.contentType || "").startsWith("image/");
}

export function attachmentCanPreview(link) {
  const contentType = link.contentType || link.asset?.contentType || "";
  return attachmentIsImage(link)
    || Boolean(officeFileType(link.displayName || link.asset?.displayName || ""))
    || (link.assetKind === "userLibraryObject" && (contentType === "application/pdf" || contentType.startsWith("text/")));
}
