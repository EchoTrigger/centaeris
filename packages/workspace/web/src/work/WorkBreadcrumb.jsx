import { Link } from "react-router";
import { useTranslation } from "../i18n";

export function WorkBreadcrumb({ parents = [], current, count = undefined }) {
  const { t } = useTranslation();
  return <nav className="workBreadcrumb" aria-label={t("work.breadcrumb")}>
    <ol>
      {parents.map(parent => <li key={parent.to.pathname || parent.to}>
        <Link to={parent.to}>{parent.label}</Link><span className="workBreadcrumbSeparator" aria-hidden="true">/</span>
      </li>)}
      <li className="workBreadcrumbCurrent">
        {parents.length ? <span aria-current="page">{current}</span> : <h1 aria-current="page">{current}</h1>}
        {count !== undefined ? <span className="workCount">{count}</span> : null}
      </li>
    </ol>
  </nav>;
}
