#!/usr/bin/env python3
"""从 app_icon.ico 派生托盘所需的四个图标变体, 以及 Web 工具的网页图标。

原图标是纯黑线条 + 透明背景, 所以四个变体都能程序化生成, 不损失线条质量:

    app_icon.ico          深色线条, 启用   —— 浅色任务栏 (也是 exe 自身的图标)
    app_icon_off.ico      深色线条, 禁用
    app_icon_inv.ico      浅色线条, 启用   —— 深色任务栏
    app_icon_inv_off.ico  浅色线条, 禁用

为什么要 inv 版本: 纯黑图标放在 Windows 深色主题的任务栏上几乎看不见。
为什么禁用态用降低不透明度而不是画一道斜杠: 托盘图标最小只有 16x16,
斜杠在这个尺寸下会糊成一团, 而"变淡"在深浅两种背景下都读得出来。

另外还生成:

    ../docs/favicon.ico   网页图标, 黑线条 + 浅色圆角底板

网页图标为什么不沿用透明版: 浏览器深色标签栏是深灰的, 纯黑线条在上面基本看不见,
和任务栏是同一个问题。但网页这边不能照搬 inv 的办法 —— `<link rel="icon">` 的
`media` 属性 Firefox 不认, 给两个图标它会挑错那一个。加一层不透明底板就与主题
无关了, 到哪都读得出来, 这也是绝大多数网站图标的做法。

依赖 Pillow。改动图标后重新跑一次:
    python assets/make-icons.py
"""

from pathlib import Path

from PIL import Image, ImageDraw

ASSETS = Path(__file__).parent
SOURCE = ASSETS / "app_icon.ico"
FAVICON = ASSETS.parent / "docs" / "favicon.ico"

# 禁用态的不透明度系数。0.35 左右在深浅背景下都还能看出形状,
# 又足够明显地区别于启用态。
DISABLED_ALPHA = 0.35

# 网页图标底板。不用纯白: 浅色标签栏本身接近白色, 纯白底板会看不出边界,
# 图标像是浮在半空。略微发灰能让轮廓立住。
PLATE_COLOR = (243, 243, 243, 255)


def load_frames(path: Path) -> dict[tuple[int, int], Image.Image]:
    """把 ICO 里每个尺寸单独取出来。

    逐尺寸取而不是拿最大的那张去缩放 —— 小尺寸(16x16)通常是单独设计过的,
    缩放会让线条发虚。
    """
    ico = Image.open(path)
    sizes = sorted(ico.info["sizes"])
    frames = {}
    for size in sizes:
        ico.size = size
        ico.load()
        frames[size] = ico.convert("RGBA").copy()
    return frames


def recolor(im: Image.Image, rgb: tuple[int, int, int] | None, alpha_scale: float) -> Image.Image:
    """替换线条颜色并缩放不透明度，保持抗锯齿边缘。"""
    out = im.copy()
    pixels = out.load()
    w, h = out.size
    for y in range(h):
        for x in range(w):
            r, g, b, a = pixels[x, y]
            if a == 0:
                continue
            nr, ng, nb = rgb if rgb else (r, g, b)
            pixels[x, y] = (nr, ng, nb, max(0, min(255, round(a * alpha_scale))))
    return out


def on_plate(im: Image.Image) -> Image.Image:
    """在线条底下垫一块浅色圆角底板。

    圆角半径按尺寸比例给, 但 16x16 上再小的半径也只剩两三个像素,
    所以下限取 2 —— 再小就看不出是圆角, 再大在小尺寸上会啃掉线条。
    """
    w, h = im.size
    radius = max(2, round(min(w, h) * 0.18))

    plate = Image.new("RGBA", im.size, (0, 0, 0, 0))
    ImageDraw.Draw(plate).rounded_rectangle([0, 0, w - 1, h - 1], radius=radius, fill=PLATE_COLOR)
    # alpha_composite 而不是 paste: 要让线条的抗锯齿边缘和底板正确混合
    return Image.alpha_composite(plate, im)


def save_ico(frames: dict[tuple[int, int], Image.Image], path: Path) -> None:
    """把各尺寸打包成一个 ICO。"""
    sizes = sorted(frames)
    largest = frames[sizes[-1]]
    others = [frames[s] for s in sizes[:-1]]
    largest.save(path, format="ICO", sizes=sizes, append_images=others)
    print(f"  {path.name:24} {path.stat().st_size:>6} 字节  尺寸 {sizes}")


def main() -> None:
    if not SOURCE.exists():
        raise SystemExit(f"找不到源图标: {SOURCE}")

    base = load_frames(SOURCE)
    print(f"源图标 {SOURCE.name}: 尺寸 {sorted(base)}")

    variants = {
        # (输出名, 线条颜色, 不透明度系数)
        "app_icon_off.ico": (None, DISABLED_ALPHA),
        "app_icon_inv.ico": ((255, 255, 255), 1.0),
        "app_icon_inv_off.ico": ((255, 255, 255), DISABLED_ALPHA),
    }

    for name, (rgb, alpha) in variants.items():
        frames = {size: recolor(im, rgb, alpha) for size, im in base.items()}
        save_ico(frames, ASSETS / name)

    # Web 工具的网页图标
    save_ico({size: on_plate(im) for size, im in base.items()}, FAVICON)

    print("完成。别忘了重新构建以把新资源编进 exe。")


if __name__ == "__main__":
    main()
