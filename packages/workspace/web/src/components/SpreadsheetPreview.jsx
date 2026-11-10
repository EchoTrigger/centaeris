import { lazy, Suspense, useEffect, useMemo, useState } from "react";
import { useTranslation } from "../i18n";
import { loadSpreadsheetPreview } from "../chat/loadSpreadsheetPreview.mjs";

const OfficePreview = lazy(() => import("./OfficePreview"));

function columnName(index) {
  let value = index;
  let name = "";
  while (value > 0) {
    value--;
    name = String.fromCharCode(65 + value % 26) + name;
    value = Math.floor(value / 26);
  }
  return name;
}

function displayValue(cell) {
  if (cell.kind !== "number") return cell.displayValue;
  const value = Number(cell.displayValue);
  if (!Number.isFinite(value)) return cell.displayValue;
  const format = String(cell.numberFormat || "");
  const decimalMatch = format.match(/\.([0#]+)/);
  const digits = decimalMatch ? decimalMatch[1].length : 0;
  const percent = format.includes("%");
  const currency = format.match(/[¥￥$€£]/)?.[0] || "";
  return `${currency}${(percent ? value * 100 : value).toLocaleString(undefined, {
    useGrouping: format.includes(","), minimumFractionDigits: digits, maximumFractionDigits: digits,
  })}${percent ? "%" : ""}`;
}

function cellStyle(style) {
  const result = {
    color: style?.font?.color || undefined,
    backgroundColor: style?.fill || undefined,
    fontWeight: style?.font?.bold ? 650 : undefined,
    fontStyle: style?.font?.italic ? "italic" : undefined,
    textAlign: ["left", "center", "right"].includes(style?.horizontal) ? style.horizontal : undefined,
    verticalAlign: ["top", "center", "bottom"].includes(style?.vertical) ? style.vertical === "center" ? "middle" : style.vertical : undefined,
    whiteSpace: style?.wrapText ? "normal" : "nowrap",
  };
  for (const side of ["top", "right", "bottom", "left"]) {
    const border = style?.borders?.[side];
    if (border) result[`border${side[0].toUpperCase()}${side.slice(1)}`] = `${border.style === "double" ? 3 : border.style?.includes("thick") ? 2 : 1}px ${border.style === "dashed" ? "dashed" : border.style === "dotted" ? "dotted" : "solid"} ${border.color || "#cfd4da"}`;
  }
  return result;
}

function Sheet({ sheet, styles }) {
  const rows = useMemo(() => new Map(sheet.rows.map((row) => [row.index, row])), [sheet]);
  const columns = useMemo(() => new Map(sheet.columns.map((column) => [column.index, column])), [sheet]);
  const mergeStarts = useMemo(() => new Map(sheet.merges.map((merge) => [`${merge.startRow}:${merge.startColumn}`, merge])), [sheet]);
  const covered = useMemo(() => {
    const values = new Set();
    for (const merge of sheet.merges) for (let row = merge.startRow; row <= merge.endRow; row++) for (let column = merge.startColumn; column <= merge.endColumn; column++) {
      if (row !== merge.startRow || column !== merge.startColumn) values.add(`${row}:${column}`);
    }
    return values;
  }, [sheet]);
  const pane = String(sheet.frozenPane || "").match(/^([A-Z]+)(\d+)$/);
  const frozenRow = pane ? Number(pane[2]) : 1;
  const frozenColumn = pane ? [...pane[1]].reduce((value, character) => value * 26 + character.charCodeAt(0) - 64, 0) : 1;
  return <div className="spreadsheetViewport">
    <table className="spreadsheetGrid" role="grid" aria-label={sheet.name}>
      <colgroup><col className="spreadsheetRowNumberColumn" />{Array.from({ length: sheet.maxColumn }, (_, index) => {
        const dimension = columns.get(index + 1);
        return <col key={index + 1} style={{ width: dimension?.hidden ? 0 : `${dimension?.widthPx || 96}px`, visibility: dimension?.hidden ? "collapse" : undefined }} />;
      })}</colgroup>
      <thead><tr><th className="spreadsheetCorner" />{Array.from({ length: sheet.maxColumn }, (_, index) => <th key={index + 1} scope="col">{columnName(index + 1)}</th>)}</tr></thead>
      <tbody>{Array.from({ length: sheet.maxRow }, (_, index) => {
        const rowIndex = index + 1;
        const row = rows.get(rowIndex);
        const cells = new Map((row?.cells || []).map((cell) => [cell.column, cell]));
        return <tr key={rowIndex} hidden={row?.hidden} style={{ height: row?.heightPx ? `${row.heightPx}px` : undefined }}>
          <th scope="row">{rowIndex}</th>
          {Array.from({ length: sheet.maxColumn }, (_, columnIndex) => {
            const column = columnIndex + 1;
            const key = `${rowIndex}:${column}`;
            if (covered.has(key)) return null;
            const cell = cells.get(column);
            const merge = mergeStarts.get(key);
            const value = cell ? displayValue(cell) : "";
            const className = `${rowIndex < frozenRow ? " isFrozenRow" : ""}${column < frozenColumn ? " isFrozenColumn" : ""}`;
            return <td key={column} role="gridcell" aria-label={value || `${columnName(column)}${rowIndex}`} className={className}
              rowSpan={merge ? merge.endRow - merge.startRow + 1 : undefined}
              colSpan={merge ? merge.endColumn - merge.startColumn + 1 : undefined}
              title={cell?.formula || undefined} style={cellStyle(styles[cell?.styleId])}>{value}</td>;
          })}
        </tr>;
      })}</tbody>
    </table>
  </div>;
}

export default function SpreadsheetPreview({ src, printSrc, title, className = "" }) {
  const { t } = useTranslation();
  const [workbook, setWorkbook] = useState(null);
  const [active, setActive] = useState(0);
  const [mode, setMode] = useState("table");
  const [error, setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    void loadSpreadsheetPreview(src, { signal: controller.signal }).then(setWorkbook).catch((failure) => {
      if (!controller.signal.aborted) setError(failure.name === "TimeoutError" ? "spreadsheetPreview.timeout" : "spreadsheetPreview.error");
    });
    return () => controller.abort();
  }, [src]);
  const sheet = workbook?.sheets[active];
  return <section className={`spreadsheetPreview ${className}`} aria-label={title}>
    <div className="spreadsheetModeSwitch" role="group" aria-label={t("spreadsheetPreview.viewMode")}>
      <button type="button" aria-pressed={mode === "table"} onClick={() => setMode("table")}>{t("spreadsheetPreview.tableView")}</button>
      <button type="button" aria-pressed={mode === "print"} onClick={() => setMode("print")}>{t("spreadsheetPreview.printView")}</button>
    </div>
    {mode === "print" ? <Suspense fallback={<div role="status">{t("officePreview.loading")}</div>}><OfficePreview src={printSrc} title={title} /></Suspense> : error ? <div className="spreadsheetState" role="alert">{t(error)}</div> : !sheet ? <div className="spreadsheetState" role="status">{t("spreadsheetPreview.loading")}</div> : <>
      <Sheet sheet={sheet} styles={workbook.styles} />
      <div className="spreadsheetTabs" role="tablist" aria-label={t("spreadsheetPreview.sheets")}>{workbook.sheets.map((candidate, index) => <button key={`${candidate.name}:${index}`} type="button" role="tab" aria-selected={index === active} onClick={() => setActive(index)}>{candidate.name}</button>)}</div>
    </>}
  </section>;
}
