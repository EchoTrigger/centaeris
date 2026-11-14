import { ChevronRight } from "lucide-react";
import { filePresentation } from "./filePresentation.mjs";

/** @param {{ filename: string, href: string, contentType?: string, id?: string, onClick?: import("react").MouseEventHandler<HTMLAnchorElement>, download?: boolean }} props */
export function FileLinkCard({ filename, href, contentType = "", id, onClick, download }) {
  const { Icon, label } = filePresentation(filename, contentType);
  return <a className="fileLinkCard" id={id} href={href} onClick={onClick} download={download} aria-label={filename}>
    <Icon className="fileLinkCardIcon" aria-hidden="true" />
    <span className="fileLinkCardCopy"><span className="fileLinkCardName">{filename}</span><small>{label}</small></span>
    <ChevronRight className="fileLinkCardChevron" aria-hidden="true" />
  </a>;
}
