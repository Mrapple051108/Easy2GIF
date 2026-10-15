"""Regenerate the vector branding and Windows icons (requires Pillow)."""
from pathlib import Path
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "app/assets"
BRAND = ROOT / "docs/brand"
PRIMARY = "#6264dd"
MINT = "#8de7d4"
E_PATH = [('M', 38, 72), ('H', 98), ('V', 94), ('H', 59), ('V', 117),
          ('H', 91), ('V', 139), ('H', 59), ('V', 162), ('H', 98),
          ('V', 184), ('H', 38), ('Z',)]
PLAY_PATH = [('M', 113, 106), ('L', 141, 126), ('Q', 144, 128, 141, 130),
             ('L', 113, 150), ('Q', 110, 152, 110, 148), ('V', 108),
             ('Q', 110, 104, 113, 106), ('Z',)]
G_PATH = [('M', 218, 101), ('C', 214, 80, 201, 70, 184, 70),
          ('C', 160, 70, 148, 87, 148, 112), ('V', 144),
          ('C', 148, 171, 160, 186, 184, 186),
          ('C', 204, 186, 219, 172, 219, 151), ('V', 127), ('H', 183),
          ('V', 148), ('H', 198), ('V', 153),
          ('C', 198, 162, 193, 167, 185, 167),
          ('C', 174, 167, 169, 160, 169, 144), ('V', 112),
          ('C', 169, 96, 173, 89, 183, 89),
          ('C', 191, 89, 196, 93, 198, 102), ('Z',)]
SHAPES = [(E_PATH, '#ffffff'), (PLAY_PATH, MINT), (G_PATH, '#ffffff')]


def mark_svg():
    body = f'<rect x="8" y="8" width="240" height="240" rx="62" fill="{PRIMARY}"/>'
    for commands, color in SHAPES:
        data = ''.join(command[0] + ' '.join(map(str, command[1:])) for command in commands)
        body += f'<path d="{data}" fill="{color}"/>'
    return body


def outline(commands):
    """Flatten the shared vector paths for a supersampled Pillow raster."""
    points = []
    for op, *values in commands:
        if op in ('M', 'L'):
            points.append(tuple(values))
        elif op == 'H':
            points.append((values[0], points[-1][1]))
        elif op == 'V':
            points.append((points[-1][0], values[0]))
        elif op in ('Q', 'C'):
            start = points[-1]
            controls = [start] + list(zip(values[::2], values[1::2]))
            # De Casteljau evaluation preserves the exact approved SVG curves.
            for step in range(1, 65):
                t = step / 64
                work = controls[:]
                while len(work) > 1:
                    work = [tuple(a * (1 - t) + b * t for a, b in zip(p, q))
                            for p, q in zip(work, work[1:])]
                points.append(work[0])
        elif op == 'Z':
            points.append(points[0])
    return points


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
    for commands, color in SHAPES:
        draw.polygon([(x * scale, y * scale) for x, y in outline(commands)], fill=color)
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
