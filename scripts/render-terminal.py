#!/usr/bin/env python3
"""Render ANSI-coloured terminal output as an SVG terminal window (for the README).

Usage:
    script -qc "limitbar --hide-account" /dev/null | python3 scripts/render-terminal.py \
        --command "limitbar" > docs/terminal.svg
"""
import argparse
import html
import re
import sys

PALETTE = {  # GitHub-dark-like terminal colours
    "fg": "#e6edf3", "bg": "#0d1117", "bar": "#161b22", "dim": "#7d8590",
    "1": None, "31": "#ff7b72", "32": "#3fb950", "33": "#d29922", "36": "#79c0ff",
}
ANSI = re.compile(r"\x1b\[([0-9;]*)m")
CHAR_W, LINE_H, PAD_X, TOP = 8.6, 20, 24, 64


def spans(line):
    """Split one line into (text, colour, bold) runs."""
    colour, bold, pos, out = None, False, 0, []
    for match in ANSI.finditer(line):
        if match.start() > pos:
            out.append((line[pos:match.start()], colour, bold))
        for code in (match.group(1) or "0").split(";"):
            if code in ("0", ""):
                colour, bold = None, False
            elif code == "1":
                bold = True
            elif code in PALETTE:
                colour = PALETTE[code]
        pos = match.end()
    if pos < len(line):
        out.append((line[pos:], colour, bold))
    return out


def render(text, command, title):
    lines = [l.rstrip("\r") for l in text.replace("\r\n", "\n").rstrip("\n").split("\n")]
    lines = [f"\x1b[36m$\x1b[0m {command}"] + lines
    width_chars = max(len(ANSI.sub("", l)) for l in lines)
    width = int(max(560, PAD_X * 2 + width_chars * CHAR_W))
    height = TOP + len(lines) * LINE_H + 20
    body = []
    for index, line in enumerate(lines):
        y = TOP + index * LINE_H
        runs = []
        for chunk, colour, bold in spans(line):
            attrs = []
            if colour:
                attrs.append(f'fill="{colour}"')
            if bold:
                attrs.append('font-weight="700"')
            runs.append(f'<tspan {" ".join(attrs)}>{html.escape(chunk)}</tspan>' if attrs else html.escape(chunk))
        body.append(f'<text x="{PAD_X}" y="{y}" xml:space="preserve">{"".join(runs)}</text>')
    dots = "".join(
        f'<circle cx="{24 + i * 20}" cy="20" r="6" fill="{c}"/>' for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"])
    )
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="{html.escape(title)}">
<title>{html.escape(title)}</title>
<rect width="{width}" height="{height}" rx="12" fill="{PALETTE["bg"]}"/>
<rect width="{width}" height="40" rx="12" fill="{PALETTE["bar"]}"/>
<rect y="28" width="{width}" height="12" fill="{PALETTE["bar"]}"/>
{dots}
<text x="{width / 2}" y="25" text-anchor="middle" fill="{PALETTE["dim"]}" font-family="ui-sans-serif, -apple-system, Segoe UI, Helvetica, Arial, sans-serif" font-size="13">{html.escape(title)}</text>
<g font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', monospace" font-size="14" fill="{PALETTE["fg"]}">
{chr(10).join(body)}
</g>
</svg>
'''


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--command", default="limitbar")
    parser.add_argument("--title", default="limitbar")
    args = parser.parse_args()
    sys.stdout.write(render(sys.stdin.read(), args.command, args.title))


if __name__ == "__main__":
    main()
