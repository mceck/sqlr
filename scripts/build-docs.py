#!/usr/bin/env python3
"""Render the Markdown guides under docs/ to HTML pages styled like docs/index.html.

Usage: python3 scripts/build-docs.py

Every .md file listed in USAGE gets a sibling .html file. Links between the pages are
rewritten from .md to .html. Only the standard library is used; the converter covers the
Markdown the docs use: headings, paragraphs, nested lists, tables, fenced code, block
quotes, rules, links, images, code spans, bold and italics.
"""

import html
import re
from pathlib import Path

GITHUB = "https://github.com/mceck/sqlr"
DOCS = Path(__file__).resolve().parent.parent / "docs"

# (path relative to docs/, title in the side navigation, group)
USAGE = [
    ("usage/README.md", "Start here"),
    ("usage/getting-started.md", "Getting started"),
    ("usage/connections.md", "Connections"),
    ("usage/explorer-and-objects.md", "Explorer and object editors"),
    ("usage/sql-editor.md", "The SQL editor"),
    ("usage/results-and-editing.md", "Results and data editing"),
    ("usage/profiles-and-transactions.md", "Profiles and transactions"),
    ("usage/moving-data.md", "Import, export, transfer"),
    ("usage/schema-tools.md", "Schema tools"),
    ("usage/administration.md", "Administration"),
    ("usage/nosql.md", "Document and key-value"),
    ("usage/drivers.md", "Drivers"),
    ("usage/settings-and-shortcuts.md", "Settings and shortcuts"),
]
PAGES = {path for path, _ in USAGE}

CSS = """
:root{--bg:#0b0e14;--bg-soft:#10151f;--panel:#141b27;--line:#243049;--text:#e8edf5;--muted:#9aa7bd;--dim:#6b7a93;--accent:#4fd1a5;--accent-soft:rgba(79,209,165,.14);--blue:#6ea8fe;--mono:"SF Mono",ui-monospace,"JetBrains Mono",Menlo,monospace;--sans:-apple-system,BlinkMacSystemFont,"Segoe UI",Inter,Roboto,sans-serif}
*{box-sizing:border-box}html{scroll-behavior:smooth}
body{margin:0;background:var(--bg);color:var(--text);font:16px/1.7 var(--sans);-webkit-font-smoothing:antialiased}
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}
.topbar{position:sticky;top:0;z-index:10;background:rgba(11,14,20,.88);backdrop-filter:blur(12px);border-bottom:1px solid var(--line)}
.topbar .in{max-width:1180px;margin:0 auto;padding:0 24px;height:56px;display:flex;align-items:center;gap:18px}
.brand{display:flex;align-items:center;gap:10px;font-weight:700;font-size:18px;color:var(--text)}
.brand img{width:26px;height:26px;border-radius:6px}.brand:hover{text-decoration:none}
.topbar .back{margin-left:auto}.topbar .back,.topbar .gh{font-size:14px;color:var(--muted)}.topbar a:hover{color:var(--text)}
.layout{max-width:1180px;margin:0 auto;padding:0 24px;display:grid;grid-template-columns:230px minmax(0,1fr);gap:48px}
aside{position:sticky;top:56px;align-self:start;height:calc(100vh - 56px);overflow:auto;padding:28px 0;font-size:14.5px}
aside h4{margin:18px 0 6px;font-size:12px;letter-spacing:.08em;text-transform:uppercase;color:var(--dim)}
aside h4:first-child{margin-top:0}
aside a{display:block;padding:5px 10px;border-radius:8px;color:var(--muted)}
aside a:hover{color:var(--text);background:var(--panel);text-decoration:none}
aside a.on{color:var(--accent);background:var(--accent-soft)}
main{padding:36px 0 80px;max-width:780px}
h1{font-size:36px;letter-spacing:-.02em;margin:0 0 18px;line-height:1.2}
h2{font-size:25px;letter-spacing:-.01em;margin:44px 0 10px;padding-top:26px;border-top:1px solid var(--line)}
h3{font-size:19px;margin:30px 0 8px}h4{font-size:16px;margin:22px 0 6px}
p{margin:0 0 14px;color:#cfd8e6}li{color:#cfd8e6}
ul,ol{margin:0 0 16px;padding-left:24px}li{margin:4px 0}li>ul,li>ol{margin:4px 0 0}
code{font-family:var(--mono);font-size:.88em;background:var(--panel);border:1px solid var(--line);border-radius:6px;padding:1px 6px;color:var(--blue)}
pre{background:var(--bg-soft);border:1px solid var(--line);border-radius:12px;padding:14px 16px;overflow-x:auto;line-height:1.55;margin:0 0 18px}
pre code{background:none;border:0;padding:0;color:var(--text);font-size:13.5px}
blockquote{margin:0 0 16px;padding:2px 16px;border-left:3px solid var(--accent);background:var(--panel);border-radius:0 10px 10px 0}
table{width:100%;border-collapse:collapse;margin:0 0 20px;font-size:14.5px;display:block;overflow-x:auto}
th,td{text-align:left;padding:9px 12px;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--muted);font-size:12.5px;text-transform:uppercase;letter-spacing:.05em;white-space:nowrap}
td{color:#cfd8e6}
hr{border:0;border-top:1px solid var(--line);margin:30px 0}
img{max-width:100%}
@media(max-width:820px){.layout{grid-template-columns:1fr;gap:0}aside{position:static;height:auto;padding:18px 0 0;border-bottom:1px solid var(--line)}h1{font-size:29px}}
"""

PAGE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · sqlr</title>
<style>{css}</style>
</head>
<body>
<div class="topbar"><div class="in">
<a class="brand" href="{root}index.html"><img src="{root}media/icon.png" alt="">sqlr</a>
<a class="back" href="{root}index.html#docs">All documentation</a>
<a class="gh" href="{github}">GitHub</a>
</div></div>
<div class="layout">
<aside>{nav}</aside>
<main>
{body}
</main>
</div>
</body>
</html>
"""


def inline(text, here):
    """Inline Markdown to HTML. `here` is the page being rendered, for link rewriting."""
    codes = []

    def stash(m):
        codes.append(f"<code>{html.escape(m.group(1))}</code>")
        return f"\x00{len(codes) - 1}\x00"

    text = re.sub(r"`([^`]+)`", stash, text)
    text = html.escape(text, quote=False)
    text = re.sub(
        r"!\[([^\]]*)\]\(([^)\s]+)\)", lambda m: f'<img src="{m.group(2)}" alt="{m.group(1)}">', text
    )
    text = re.sub(
        r"\[([^\]]+)\]\(([^)\s]+)\)",
        lambda m: f'<a href="{rewrite(m.group(2), here)}">{m.group(1)}</a>',
        text,
    )
    text = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", text)
    text = re.sub(r"(?<![\w*])\*(?!\s)(.+?)(?<!\s)\*(?![\w*])", r"<em>\1</em>", text)
    return re.sub(r"\x00(\d+)\x00", lambda m: codes[int(m.group(1))], text)


def rewrite(href, here):
    """Point a link to a rendered page at its .html file."""
    if re.match(r"[a-z]+:|#", href):
        return href
    path, _, frag = href.partition("#")
    if path.endswith(".md"):
        try:
            rel = (here.parent / path).resolve().relative_to(DOCS).as_posix()
        except ValueError:
            rel = None
        if rel in PAGES:
            path = path[:-3] + ".html"
    return path + ("#" + frag if frag else "")


def slug(text):
    text = re.sub(r"<[^>]+>", "", text).lower()
    return re.sub(r"[^\w\s-]", "", text).strip().replace(" ", "-")


def split_row(line):
    return [c.strip() for c in line.strip().strip("|").split("|")]


def render(md, here):
    lines = md.splitlines()
    out, i = [], 0
    title = None

    def list_block(i, indent):
        """Render the list starting at lines[i] whose markers sit at `indent` spaces."""
        marker = re.compile(r"^(\s*)([-*]|\d+\.)\s+(.*)")
        m = marker.match(lines[i])
        tag = "ol" if m.group(2)[0].isdigit() else "ul"
        items = [f"<{tag}>"]
        while i < len(lines):
            m = marker.match(lines[i])
            if not m or len(m.group(1)) != indent:
                break
            text = [m.group(3)]
            i += 1
            sub = ""
            while i < len(lines) and lines[i].strip():
                m2 = marker.match(lines[i])
                if m2 and len(m2.group(1)) > indent:
                    html_sub, i = list_block(i, len(m2.group(1)))
                    sub += html_sub
                elif m2:
                    break
                else:
                    text.append(lines[i].strip())
                    i += 1
            items.append(f"<li>{inline(' '.join(text), here)}{sub}</li>")
        items.append(f"</{tag}>")
        return "".join(items), i

    while i < len(lines):
        line = lines[i]
        if not line.strip():
            i += 1
        elif line.startswith("```"):
            i += 1
            code = []
            while i < len(lines) and not lines[i].startswith("```"):
                code.append(lines[i])
                i += 1
            i += 1
            out.append(f"<pre><code>{html.escape(chr(10).join(code))}</code></pre>")
        elif m := re.match(r"^(#{1,4})\s+(.*)", line):
            level, text = len(m.group(1)), m.group(2)
            rendered = inline(text, here)
            if level == 1 and title is None:
                title = re.sub(r"<[^>]+>", "", rendered)
            out.append(f'<h{level} id="{slug(rendered)}">{rendered}</h{level}>')
            i += 1
        elif re.match(r"^(-{3,}|\*{3,})\s*$", line):
            out.append("<hr>")
            i += 1
        elif line.startswith(">"):
            quote = []
            while i < len(lines) and lines[i].startswith(">"):
                quote.append(lines[i][1:].strip())
                i += 1
            out.append(f"<blockquote><p>{inline(' '.join(quote), here)}</p></blockquote>")
        elif line.lstrip().startswith("|") and i + 1 < len(lines) and re.match(r"^\s*\|[\s:|-]+\|\s*$", lines[i + 1]):
            head = split_row(line)
            i += 2
            rows = []
            while i < len(lines) and lines[i].lstrip().startswith("|"):
                rows.append(split_row(lines[i]))
                i += 1
            t = "<table><thead><tr>" + "".join(f"<th>{inline(c, here)}</th>" for c in head) + "</tr></thead><tbody>"
            for r in rows:
                t += "<tr>" + "".join(f"<td>{inline(c, here)}</td>" for c in r) + "</tr>"
            out.append(t + "</tbody></table>")
        elif re.match(r"^\s*([-*]|\d+\.)\s+", line):
            block, i = list_block(i, len(line) - len(line.lstrip()))
            out.append(block)
        else:
            para = []
            while i < len(lines) and lines[i].strip() and not re.match(
                r"^(```|#{1,4}\s|>|\s*([-*]|\d+\.)\s+|\s*\|)", lines[i]
            ):
                para.append(lines[i].strip())
                i += 1
            if not para:  # a stray line that matched no block rule
                para.append(lines[i].strip())
                i += 1
            out.append(f"<p>{inline(' '.join(para), here)}</p>")
    return "\n".join(out), title or here.stem


def nav(current, root):
    def group(name, entries):
        links = "".join(
            f'<a class="{"on" if path == current else ""}" href="{root}{path[:-3]}.html">{label}</a>'
            for path, label in entries
        )
        return f"<h4>{name}</h4>{links}"

    return group("Using sqlr", USAGE) + f'<h4>Project</h4><a href="{GITHUB}">Source on GitHub</a>'


def main():
    for path in sorted(PAGES):
        src = DOCS / path
        body, title = render(src.read_text(), src)
        root = "../" * path.count("/")
        page = PAGE.format(title=html.escape(title), css=CSS, root=root, github=GITHUB, nav=nav(path, root), body=body)
        src.with_suffix(".html").write_text(page)
        print(f"docs/{path[:-3]}.html")


if __name__ == "__main__":
    main()
