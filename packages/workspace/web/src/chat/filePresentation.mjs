import { File, FileArchive, FileAudio, FileCodeCorner, FileSpreadsheet, FileText, FileVideo } from "lucide-react";

export function filePresentation(name, contentType = "") {
  const extension = name.split(".").at(-1)?.toLowerCase();
  const label = name.includes(".") && /^[a-z0-9]{1,6}$/.test(extension) ? extension.toUpperCase() : "FILE";
  const Icon = /^(csv|xlsx?|ods)$/.test(extension) ? FileSpreadsheet
    : /^(zip|rar|7z|tar|gz)$/.test(extension) ? FileArchive
      : contentType.startsWith("audio/") ? FileAudio
        : contentType.startsWith("video/") ? FileVideo
          : /^(json|html|css|js|ts|jsx|tsx|py|rs|xml|yaml|yml|ps1)$/.test(extension) ? FileCodeCorner
            : contentType.startsWith("text/") || /^(pdf|docx?|odt|rtf)$/.test(extension) ? FileText : File;
  return { Icon, label };
}
