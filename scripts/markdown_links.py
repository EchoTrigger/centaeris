"""Check repository Markdown destinations, not anchors or document semantics.

Supports inline links/images (bare or angle-bracket destinations, optional
titles, and one level of parentheses) and single-line reference definitions.
Fenced and inline code is ignored. This is not a full CommonMark parser: HTML
links, indented code blocks, and deeply nested destinations are out of scope.
"""
from collections.abc import Iterable, Iterator
from pathlib import Path
import re
import subprocess
from urllib.parse import unquote, urlsplit


DESTINATION = r"(?:<(?P<angle>[^>\n]+)>|(?P<bare>(?:\\.|[^\s()\\]|\([^()\n]*\))+))"
INLINE_LINK = re.compile(
    r"\]\(\s*" + DESTINATION + r'''\s*(?:(?:"[^"\n]*"|'[^'\n]*'|\([^\n)]*\))\s*)?\)'''
)
REFERENCE = re.compile(r"^ {0,3}\[[^\]\n]+\]:[ \t]*" + DESTINATION, re.MULTILINE)
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})(.*)$")
INLINE_CODE = re.compile(r"(?<!`)(`+)(?!`)(.*?)(?<!`)\1(?!`)", re.DOTALL)


def markdown_sources(root: Path) -> list[Path]:
    """Include tracked and new nonignored docs without scanning build output."""
    paths = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "*.md"], cwd=root
    ).decode("utf-8").split("\0")
    sources = []
    for name in sorted(set(paths) - {""}):
        path = Path(name)
        if len(path.parts) != 1 and path.parts[0] not in {"docs", ".github"}:
            continue
        if path.name == "AGENTS.md" or path.parts[:3] == ("docs", "workspace", "source-snapshot"):
            continue
        if (root / path).is_file():
            sources.append(root / path)
    return sources


def local_markdown_links(source: Path) -> Iterator[tuple[Path, str]]:
    """Yield the source and extracted destination for each local file link."""
    lines = []
    fence = ""
    for line in source.read_text(encoding="utf-8").splitlines(keepends=True):
        marker = FENCE.match(line)
        if fence:
            if marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence) and not marker[2].strip():
                fence = ""
            lines.append("\n")
        elif marker:
            fence = marker[1]
            lines.append("\n")
        else:
            lines.append(line)
    text = INLINE_CODE.sub("", "".join(lines))
    for pattern in (INLINE_LINK, REFERENCE):
        for match in pattern.finditer(text):
            destination = match["angle"] or match["bare"]
            destination = re.sub(r"\\([\\`*{}\[\]()#+.!_<>-])", r"\1", destination)
            parsed = urlsplit(destination)
            if not parsed.scheme and not parsed.netloc and parsed.path:
                yield source, unquote(parsed.path)


def missing_local_links(sources: Iterable[Path], root: Path) -> list[str]:
    missing = []
    for source in sources:
        for _, destination in local_markdown_links(source):
            target = root / destination.lstrip("/") if destination.startswith("/") else source.parent / destination
            if not target.exists():
                missing.append(f"{source.relative_to(root).as_posix()} -> {destination}")
    return missing
