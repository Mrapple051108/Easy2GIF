"""Regenerate the vector branding and Windows icons (requires Pillow)."""
from pathlib import Path
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "app/assets"
BRAND = ROOT / "docs/brand"
PRIMARY = "#6264dd"
MINT = "#8de7d4"
E_POINTS = [(64, 64), (160, 64), (160, 92), (92, 92), (92, 114),
            (138, 114), (138, 142), (92, 142), (92, 164), (160, 164),
            (160, 192), (64, 192)]
PLAY_POINTS = [(160, 104), (200, 128), (160, 152)]


def mark_svg():
    points = lambda items: " ".join(f"{x},{y}" for x, y in items)
    return (f'<rect x="8" y="8" width="240" height="240" rx="62" fill="{PRIMARY}"/>'
            f'<polygon points="{points(E_POINTS)}" fill="#ffffff"/>'
            f'<polygon points="{points(PLAY_POINTS)}" fill="{MINT}" stroke="{MINT}" '
            'stroke-width="4" stroke-linejoin="round"/>')


def write_svg(path, width, height, body, label):
    path.write_text(
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img" aria-label="{label}">'
        f'<title>{label}</title>{body}</svg>\n', encoding="utf-8")


def icon_png():
    scale = 8
    image = Image.new("RGBA", (256 * scale, 256 * scale))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle(tuple(v * scale for v in (8, 8, 248, 248)),
                           radius=62 * scale, fill=PRIMARY)
    draw.polygon([(x * scale, y * scale) for x, y in E_POINTS], fill="white")
    points = [(x * scale, y * scale) for x, y in PLAY_POINTS]
    draw.polygon(points, fill=MINT)
    draw.line(points + [points[0]], fill=MINT, width=4 * scale, joint="curve")
    for x, y in points:
        draw.ellipse((x - 2 * scale, y - 2 * scale, x + 2 * scale, y + 2 * scale), fill=MINT)
    return image.resize((512, 512), Image.Resampling.LANCZOS)


def badge(filename, label, value, left, right):
    width = left + right
    body = (f'<clipPath id="r"><rect width="{width}" height="28" rx="6"/></clipPath>'
            f'<g clip-path="url(#r)"><path fill="#303447" d="M0 0h{left}v28H0z"/>'
            f'<path fill="{PRIMARY}" d="M{left} 0h{right}v28H{left}z"/></g>'
            '<g fill="#ffffff" font-family="Verdana,Arial,sans-serif" font-size="11" '
            'text-anchor="middle">'
            f'<text x="{left / 2}" y="18">{label}</text>'
            f'<text x="{left + right / 2}" y="18">{value}</text></g>')
    write_svg(BRAND / filename, width, 28, body, f"{label}: {value}")


def main():
    ASSETS.mkdir(parents=True, exist_ok=True)
    BRAND.mkdir(parents=True, exist_ok=True)
    write_svg(ASSETS / "easy2gif.svg", 256, 256, mark_svg(), "Easy2GIF")
    image = icon_png()
    image.save(ASSETS / "easy2gif.png", optimize=True)
    image.save(ASSETS / "easy2gif.ico", sizes=[(n, n) for n in (16, 20, 24, 32, 40, 48, 64, 128, 256)])
    for name, ink in (("light", "#252a3b"), ("dark", "#eff1f7")):
        body = ('<g transform="translate(0 4) scale(.46875)">' + mark_svg() + '</g>'
                f'<text x="140" y="84" fill="{ink}" font-family="Segoe UI,Arial,sans-serif" '
                'font-size="58" font-weight="650" letter-spacing="-2">Easy'
                f'<tspan fill="{PRIMARY if name == "light" else "#a0a5ff"}">2</tspan>GIF</text>')
        write_svg(BRAND / f"logo-{name}.svg", 460, 128, body, "Easy2GIF")
    badge("platform.svg", "Windows", "x64", 74, 42)
    badge("stack.svg", "Rust", "Qt Quick", 48, 76)
    badge("local.svg", "Processing", "Local", 82, 52)


if __name__ == "__main__":
    main()
