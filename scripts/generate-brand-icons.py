# /// script
# requires-python = ">=3.11"
# dependencies = ["cairosvg==2.8.2"]
# ///
"""Regenerate Cairn raster icons from the committed Balise SVG sources.

Run: uv run scripts/generate-brand-icons.py
"""
from pathlib import Path
import cairosvg

PUBLIC = Path(__file__).resolve().parents[1] / 'apps/web/public'
source = (PUBLIC / 'brand/cairn-app.svg').read_text()
for name, size in [('icon-192.png', 192), ('icon-512.png', 512), ('apple-touch-icon.png', 180)]:
    cairosvg.svg2png(bytestring=source.encode(), write_to=str(PUBLIC / 'icons' / name), output_width=size, output_height=size)

# Android/PWA adaptive masks keep the mark within the central safe zone.
maskable = source.replace('rx="5.6"', '').replace('scale(0.72)', 'scale(0.60)')
cairosvg.svg2png(bytestring=maskable.encode(), write_to=str(PUBLIC / 'icons/icon-maskable-512.png'), output_width=512, output_height=512)
cairosvg.svg2png(url=str(PUBLIC / 'brand/cairn-social.svg'), write_to=str(PUBLIC / 'brand/cairn-social.png'), output_width=1200, output_height=630)

cairosvg.svg2png(url=str(PUBLIC / "brand/cairn-wordmark.svg"), write_to=str(PUBLIC / "brand/cairn-wordmark.png"), output_width=312)
