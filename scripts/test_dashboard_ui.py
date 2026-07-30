#!/usr/bin/env python3
"""Validate the browser shell's accessibility, privacy, and API contract."""

from __future__ import annotations

import re
from html.parser import HTMLParser
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class DashboardParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.ids: set[str] = set()
        self.labels: set[str] = set()
        self.inline_handlers: list[str] = []
        self.inline_styles = 0
        self.inline_scripts = 0
        self.external_urls: list[str] = []
        self._script_without_src = False

    def handle_starttag(
        self, tag: str, attributes: list[tuple[str, str | None]]
    ) -> None:
        values = dict(attributes)
        if identifier := values.get("id"):
            self.ids.add(identifier)
        if tag == "label" and (target := values.get("for")):
            self.labels.add(target)
        self.inline_handlers.extend(
            name for name, _ in attributes if name.lower().startswith("on")
        )
        if "style" in values:
            self.inline_styles += 1
        if tag == "script" and "src" not in values:
            self.inline_scripts += 1
            self._script_without_src = True
        for name in ("src", "href"):
            value = values.get(name) or ""
            if value.startswith(("http://", "https://", "//")):
                self.external_urls.append(value)


def main() -> int:
    html = (ROOT / "dashboard/index.html").read_text()
    javascript = (ROOT / "dashboard/app.js").read_text()
    css = (ROOT / "dashboard/styles.css").read_text()
    parser = DashboardParser()
    parser.feed(html)

    assert not parser.inline_handlers
    assert parser.inline_styles == 0
    assert parser.inline_scripts == 0
    assert not parser.external_urls
    controls = set(
        re.findall(r"<(?:input|select|textarea)\b[^>]*\bid=\"([^\"]+)\"", html)
    )
    assert controls <= parser.labels, sorted(controls - parser.labels)
    for view in [
        "overview",
        "search",
        "research",
        "impact",
        "trace",
        "workspaces",
        "sessions",
        "recaps",
        "memory",
    ]:
        assert f'data-view-panel="{view}"' in html
    for endpoint in [
        "health",
        "pair",
        "status",
        "search",
        "research",
        "impact",
        "trace",
        "workspaces",
        "sessions",
        "recap",
        "memory",
    ]:
        assert f'/api/v1/{endpoint}"' in javascript

    assert 'sessionStorage.getItem("structurely.token")' in javascript
    assert 'localStorage.getItem("structurely.bridgeUrl")' in javascript
    assert 'localStorage.setItem("structurely.token"' not in javascript
    assert 'targetAddressSpace: "loopback"' in javascript
    assert "https://" not in javascript
    assert "@media" in css
    assert "prefers-reduced-motion" in css
    assert ":focus-visible" in css
    assert "overflow-x" in css
    assert '<meta name="theme-color" content="#1c1e54">' in html
    assert 'aria-current="page"' in html
    assert 'aria-controls="primary-navigation"' in html
    assert 'font-feature-settings: "ss01"' in css
    assert 'font-feature-settings: "tnum"' in css
    for token in [
        "--bg: #11133b",
        "--panel: #1c1e54",
        "--accent: #665efd",
        "--accent-press: #4434d4",
        "--cream: #f5e9d4",
        "border-radius: 9999px",
        "font-weight: 300",
        "@media (max-width: 1023px)",
        "@media (max-width: 767px)",
        "min-height: 44px",
    ]:
        assert token in css, token
    assert "https://" not in css
    print("Dashboard UI accessibility, privacy, and API contract passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
