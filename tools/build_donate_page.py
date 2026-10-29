"""Builds the donate page in site/ from the funding file.

Reads the two addresses from .github/FUNDING.yml (the line marked "# BCH"
and the line marked "# CashTokens"; from origin/master when the file is not
in this checkout), draws their QR codes as inline SVG, and writes
site/index.html and site/.nojekyll. Prints no address.

Run from the repository root: python tools/build_donate_page.py
"""
import html
import pathlib
import re
import subprocess

import qrcode

ROOT = pathlib.Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
FUNDING = ROOT / ".github" / "FUNDING.yml"

if FUNDING.exists():
    funding = FUNDING.read_text(encoding="utf-8")
else:
    funding = subprocess.run(
        ["git", "-C", str(ROOT), "show", "origin/master:.github/FUNDING.yml"],
        capture_output=True, text=True, check=True,
    ).stdout


def address(marker: str) -> str:
    for line in funding.splitlines():
        if marker in line:
            found = re.search(r'"(bitcoincash:[qpz][a-z0-9]{38,})"', line)
            if found:
                return found.group(1)
    raise SystemExit(f"no address marked {marker!r} in FUNDING.yml")


bch = address("# BCH")
tokens = address("# CashTokens")
assert bch.startswith("bitcoincash:q") or bch.startswith("bitcoincash:p")
assert tokens.startswith("bitcoincash:z") or tokens.startswith("bitcoincash:r")


def qr_svg(text: str, label: str) -> str:
    code = qrcode.QRCode(error_correction=qrcode.constants.ERROR_CORRECT_M, border=4)
    code.add_data(text)
    code.make(fit=True)
    matrix = code.get_matrix()
    size = len(matrix)
    path = []
    for y, row in enumerate(matrix):
        x = 0
        while x < size:
            if row[x]:
                start = x
                while x < size and row[x]:
                    x += 1
                path.append(f"M{start} {y}h{x - start}v1h{start - x}z")
            else:
                x += 1
    return (
        f'<svg class="qr" viewBox="0 0 {size} {size}" role="img" '
        f'aria-label="{html.escape(label)}" shape-rendering="crispEdges">'
        f'<rect width="{size}" height="{size}" fill="#fff"/>'
        f'<path fill="#000" d="{"".join(path)}"/></svg>'
    )


def card(title: str, hint: str, uri: str) -> str:
    shown = html.escape(uri)
    return f"""      <article class="card">
        <h2>{html.escape(title)}</h2>
        <p class="hint">{hint}</p>
        <button class="qr-button" type="button" data-copy="{shown}" aria-label="Copy the {html.escape(title)} address">
          {qr_svg(uri, f"QR code for the {title} address")}
        </button>
        <button class="address" type="button" data-copy="{shown}" aria-label="Copy the {html.escape(title)} address">{shown}</button>
        <p class="copied" aria-live="polite"></p>
        <a class="button" href="{shown}">Open in wallet</a>
      </article>"""


page = f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Support Pickaxe</title>
  <meta name="description" content="Send BCH or CashTokens to support Pickaxe Miner, free open-source mining software for Bitcoin Cash.">
  <meta name="color-scheme" content="light dark">
  <style>
    :root {{
      --bg: #f6f7f9; --card: #ffffff; --text: #14171c; --muted: #5b6370;
      --line: #e3e6eb; --accent: #0ac18e; --accent-text: #04241a;
    }}
    @media (prefers-color-scheme: dark) {{
      :root {{
        --bg: #0f1216; --card: #171b21; --text: #eef1f5; --muted: #9aa3b0;
        --line: #262c35; --accent: #0ac18e; --accent-text: #04241a;
      }}
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0; background: var(--bg); color: var(--text);
      font: 16px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
    }}
    main {{ max-width: 860px; margin: 0 auto; padding: 40px 16px 32px; }}
    header {{ text-align: center; margin-bottom: 28px; }}
    h1 {{ font-size: 2rem; margin: 0 0 8px; }}
    header p {{ color: var(--muted); margin: 0 auto; max-width: 560px; }}
    .cards {{ display: grid; gap: 20px; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); }}
    .card {{
      background: var(--card); border: 1px solid var(--line); border-radius: 16px;
      padding: 24px; display: flex; flex-direction: column; align-items: center; text-align: center;
    }}
    h2 {{ margin: 0; font-size: 1.35rem; }}
    .hint {{ color: var(--muted); margin: 4px 0 16px; font-size: .95rem; }}
    .qr-button {{
      border: 0; padding: 0; background: none; cursor: copy; width: 100%; max-width: 240px;
      border-radius: 12px; line-height: 0;
    }}
    .qr {{ width: 100%; height: auto; border-radius: 12px; }}
    .address {{
      font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: .85rem;
      color: var(--text); text-align: center; word-break: break-all; cursor: copy;
      user-select: all; -webkit-user-select: all;
      background: var(--bg); border: 1px solid var(--line); border-radius: 10px;
      padding: 10px 12px; margin: 16px 0 4px; width: 100%;
    }}
    .address:hover {{ filter: brightness(0.97); }}
    .qr-button:hover {{ box-shadow: 0 0 0 3px var(--line); }}
    .address:focus-visible, .qr-button:focus-visible {{ outline: 3px solid var(--accent); outline-offset: 3px; }}
    .copied {{ min-height: 1.5em; margin: 0 0 12px; color: var(--accent); font-weight: 600; font-size: .9rem; }}
    .button {{
      display: inline-block; background: var(--accent); color: var(--accent-text);
      font-weight: 600; text-decoration: none; padding: 12px 22px; border-radius: 999px;
    }}
    .button:hover {{ filter: brightness(1.05); }}
    .button:focus-visible {{ outline: 3px solid var(--text); outline-offset: 3px; }}
    .how {{ color: var(--muted); text-align: center; margin: 28px auto 0; max-width: 560px; font-size: .95rem; }}
    footer {{ text-align: center; margin-top: 24px; font-size: .9rem; }}
    footer a {{ color: var(--muted); }}
  </style>
</head>
<body>
  <main>
    <header>
      <h1>Support Pickaxe</h1>
      <p>Pickaxe Miner is free, open-source mining software for Bitcoin Cash. Donations keep its development going.</p>
    </header>
    <section class="cards">
{card("BCH", "For Bitcoin Cash.", bch)}
{card("Tokens", "For CashTokens, and BCH too.", tokens)}
    </section>
    <p class="how">Click or tap a code or an address to copy it, scan a code with your wallet, or tap <strong>Open in wallet</strong>.</p>
    <footer><a href="https://github.com/CyberAshven/pickaxe-miner">Pickaxe Miner on GitHub</a></footer>
  </main>
  <script>
    // Copies an address when its code or text is clicked (browsers allow
    // clipboard writes only from script). Without clipboard access, the
    // address is selected so Ctrl+C or the system Copy works.
    for (const button of document.querySelectorAll("[data-copy]")) {{
      button.addEventListener("click", async () => {{
        const card = button.closest(".card");
        const note = card.querySelector(".copied");
        try {{
          await navigator.clipboard.writeText(button.dataset.copy);
          note.textContent = "Copied";
        }} catch {{
          const range = document.createRange();
          range.selectNodeContents(card.querySelector(".address"));
          const selection = window.getSelection();
          selection.removeAllRanges();
          selection.addRange(range);
          note.textContent = "Selected: press Ctrl+C to copy";
        }}
        clearTimeout(note.timer);
        note.timer = setTimeout(() => {{ note.textContent = ""; }}, 2500);
      }});
    }}
  </script>
</body>
</html>
"""

SITE.mkdir(exist_ok=True)
(SITE / "index.html").write_text(page, encoding="utf-8", newline="\n")
(SITE / ".nojekyll").write_text("", encoding="utf-8")
print("built site/index.html")
