// The picked folders as a list, each with a Remove button, paths shortened with `~`.
import { useTranslation } from "react-i18next";
import "./folders.css";
import { tildePath, useHomeDir } from "./folders";

export function ShortPath({ path }: { path: string }) {
  const home = useHomeDir();
  return (
    <span className="mono" title={path}>
      {tildePath(path, home)}
    </span>
  );
}

export default function FolderList({
  folders,
  onRemove,
  disabled,
}: {
  folders: string[];
  onRemove: (folder: string) => void;
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  const home = useHomeDir();
  if (folders.length === 0) return null;
  return (
    <ul className="folder-list">
      {folders.map((folder) => {
        const short = tildePath(folder, home);
        return (
          <li key={folder}>
            <span className="mono" title={folder}>
              {short}
            </span>
            <button
              type="button"
              className="btn plain"
              disabled={disabled}
              aria-label={t("folders.remove", "Remove {{folder}}", { folder: short })}
              onClick={() => onRemove(folder)}
            >
              {t("folders.removeShort", "Remove")}
            </button>
          </li>
        );
      })}
    </ul>
  );
}
