#!/usr/bin/env python3
"""生成 qview 应用图标（.icns）。

设计：深色圆角方块 + 若干级别色的“日志行”横条（对应 Dark Pro 主题调色）。
输出：gui/macos/icon.icns，并生成 iconset 中间产物。
"""

import os
import subprocess
import sys

from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# 主题调色（Dark Pro 近似）
BG = (30, 30, 46)        # 底色
BG2 = (40, 42, 62)       # 渐变浅端
BORDER = (70, 74, 100)   # 描边
LINES = [
    (243, 139, 168),  # 红   error
    (249, 226, 175),  # 黄   warn
    (166, 227, 161),  # 绿   info
    (137, 220, 235),  # 青   debug
]
HILITE = (249, 226, 102)  # 搜索高亮（黄橙）


def make_base(size: int) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # 圆角背景（垂直渐变）
    r = int(size * 0.22)
    grad = Image.new("RGBA", (size, size), BG)
    gd = ImageDraw.Draw(grad)
    for y in range(size):
        t = y / size
        col = tuple(int(BG[i] + (BG2[i] - BG[i]) * t) for i in range(3)) + (255,)
        gd.line([(0, y), (size, y)], fill=col)
    mask = Image.new("L", (size, size), 0)
    md = ImageDraw.Draw(mask)
    md.rounded_rectangle([0, 0, size - 1, size - 1], radius=r, fill=255)
    img.paste(grad, (0, 0), mask)

    # 细描边
    d.rounded_rectangle([2, 2, size - 3, size - 3], radius=r, outline=BORDER, width=max(2, size // 256))

    # 日志行（等宽横条，对齐左留白）
    margin = int(size * 0.14)
    lh = int(size * 0.055)
    gap = int(size * 0.088)
    n = len(LINES)
    top = int(size * 0.24)
    for i, color in enumerate(LINES):
        y = top + i * gap
        # 行号占位（右侧）
        d.rectangle([margin, y, margin + lh, y + lh], fill=(90, 95, 130))
        # 文本条（渐变宽度模拟不等长日志）
        w = int(size * (0.52 + 0.08 * i))
        d.rounded_rectangle(
            [margin + lh + int(size * 0.04), y, margin + lh + int(size * 0.04) + w, y + lh],
            radius=lh // 2,
            fill=color,
        )

    # 搜索命中高亮（压在第二行上的黄橙块）
    hy = top + gap + int(size * 0.005)
    hx = margin + lh + int(size * 0.04) + int(size * 0.18)
    d.rounded_rectangle(
        [hx, hy, hx + int(size * 0.18), hy + lh],
        radius=lh // 2,
        fill=HILITE,
    )
    return img


def main() -> None:
    iconset = os.path.join(ROOT, "icon.iconset")
    os.makedirs(iconset, exist_ok=True)
    base = make_base(1024)
    sizes = [
        ("icon_16x16.png", 16),
        ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32),
        ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128),
        ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256),
        ("icon_256x256@2x.png", 512),
        ("icon_512x512.png", 512),
        ("icon_512x512@2x.png", 1024),
    ]
    for name, s in sizes:
        base.resize((s, s), Image.LANCZOS).save(os.path.join(iconset, name))
    icns = os.path.join(ROOT, "icon.icns")
    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", icns], check=True)
    # 清理中间产物
    subprocess.run(["rm", "-rf", iconset], check=False)
    print(f"生成图标: {icns} ({os.path.getsize(icns)} bytes)")


if __name__ == "__main__":
    main()
