# -*- coding: utf-8 -*-
"""
Sinh bo video test cho S1.3 — hai ca trong dinh nghia hoan thanh:

  ui-work.mp4   ~2 phut thao tac UI  -> phai ra 8-15 frame
  scroll.mp4    30 giay cuon lien tuc -> doan do phai ra <= ~6 frame

Vi sao khong dung mau tuong phan cao: hai man hinh UI deu nen sang thi diem
scene THAP. Test bang mau do/xanh la tu lua minh — no lam bo chon frame trong
de hon thuc te rat nhieu. Bo nay dung khung giong UI that: nen sang, thanh ben,
cac dong chu.

Chay:  python make-corpus.py <thu-muc-ra> <duong-dan-ffmpeg>
"""
import os
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFont

W, H = 1280, 720
BG, PANEL, INK, DIM, ACCENT = (250, 249, 247), (243, 241, 238), (41, 37, 36), (150, 143, 138), (117, 103, 168)


def font(sz):
    for p in (r"C:\Windows\Fonts\consola.ttf", r"C:\Windows\Fonts\segoeui.ttf"):
        try:
            return ImageFont.truetype(p, sz)
        except OSError:
            pass
    return ImageFont.load_default(size=sz)


F, FS = font(16), font(13)


def screen(title, lines, highlight=None, sidebar_sel=0):
    """Mot man hinh kieu IDE: sidebar + vung noi dung + thanh trang thai."""
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    d.rectangle([0, 0, 200, H], fill=PANEL)
    d.line([200, 0, 200, H], fill=(226, 223, 216))
    for i, name in enumerate(["core", "  select.rs", "  dedup.rs", "  probe.rs", "tray", "mcp", "docs"]):
        y = 44 + i * 30
        if i == sidebar_sel:
            d.rectangle([8, y - 5, 192, y + 21], fill=(234, 230, 244))
        d.text((20, y), name, font=FS, fill=INK if i == sidebar_sel else DIM)
    d.rectangle([0, 0, W, 40], fill=PANEL)
    d.line([0, 40, W, 40], fill=(226, 223, 216))
    d.text((216, 12), title, font=FS, fill=DIM)
    for i, line in enumerate(lines):
        y = 64 + i * 26
        if highlight is not None and i == highlight:
            d.rectangle([210, y - 4, W - 20, y + 22], fill=(246, 243, 252))
        d.text((216, y), f"{i + 1:>3}", font=FS, fill=(196, 190, 184))
        d.text((262, y), line, font=F, fill=INK)
    d.rectangle([0, H - 28, W, H], fill=PANEL)
    d.text((216, H - 24), "Rust   UTF-8   LF", font=FS, fill=DIM)
    return img


BASE = [
    "pub fn extract(tools: &Toolchain, video: &Path)",
    "    -> Result<Vec<SelectedFrame>, SelectError> {",
    "    params.validate()?;",
    "    let pattern = out_dir.join(\"frame-%05d.png\");",
    "",
    "    let args = vec![",
    "        \"-i\".into(), video.as_os_str().to_owned(),",
    "        \"-vf\".into(), params.filter_expr().into(),",
    "    ];",
    "",
    "    let out = tools.run_ffmpeg(&args)?;",
    "}",
]


def variants():
    """12 man hinh khac nhau — moi cai la mot 'thao tac' cua nguoi dung."""
    out = []
    for i in range(12):
        lines = list(BASE)
        # Moi buoc doi mot it: sua dong, them dong, doi file dang mo.
        if i % 3 == 0:
            lines[4] = f"    // step {i}: checked {i * 7} frames"
        if i % 4 == 1:
            lines.insert(5, f"    let threshold = 0.{10 + i};")
        if i % 4 == 2:
            lines[2] = "    params.validate().map_err(SelectError::BadParams)?;"
        out.append(screen(f"select.rs  -  framekeep-core  ({i + 1}/12)", lines,
                          highlight=i % len(lines), sidebar_sel=i % 7))
    return out


def tall_page():
    """Trang dai de cuon — nhieu dong chu, giong tai lieu that."""
    tall = Image.new("RGB", (W, H * 4), BG)
    d = ImageDraw.Draw(tall)
    for i in range(H * 4 // 26):
        y = 20 + i * 26
        width = 300 + (i * 137) % 700
        d.rectangle([80, y, 80 + width, y + 13], fill=(214, 209, 203) if i % 7 else (188, 182, 176))
    return tall


def main():
    out_dir, ffmpeg = sys.argv[1], sys.argv[2]
    frames_dir = os.path.join(out_dir, "_screens")
    os.makedirs(frames_dir, exist_ok=True)

    for i, im in enumerate(variants()):
        im.save(os.path.join(frames_dir, f"s{i:02d}.png"))
    tall_page().save(os.path.join(out_dir, "_page.png"))

    # 1. ~2 phut: 12 man hinh, moi cai dung yen 10 giay
    concat = os.path.join(out_dir, "_concat.txt")
    with open(concat, "w", encoding="utf-8") as f:
        for i in range(12):
            f.write(f"file '{os.path.join(frames_dir, f's{i:02d}.png')}'\n")
            f.write("duration 10\n")
        f.write(f"file '{os.path.join(frames_dir, 's11.png')}'\n")
    run([ffmpeg, "-y", "-hide_banner", "-loglevel", "error", "-f", "concat", "-safe", "0",
         "-i", concat, "-r", "30", "-pix_fmt", "yuv420p", "-c:v", "libopenh264",
         os.path.join(out_dir, "ui-work.mp4")])

    # 2. 30 giay cuon lien tuc — moi frame khac frame truoc mot chut
    run([ffmpeg, "-y", "-hide_banner", "-loglevel", "error", "-loop", "1",
         "-i", os.path.join(out_dir, "_page.png"), "-t", "30", "-r", "30",
         "-vf", f"scroll=vertical=0.0016,crop={W}:{H}:0:0", "-pix_fmt", "yuv420p",
         "-c:v", "libopenh264", os.path.join(out_dir, "scroll.mp4")])

    for name in ("ui-work.mp4", "scroll.mp4"):
        p = os.path.join(out_dir, name)
        print(f"  {name:<16} {os.path.getsize(p) / 1048576:6.2f} MB")


def run(cmd):
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        print("FAILED:", " ".join(cmd[:6]), "...")
        print(r.stderr[-1500:])
        sys.exit(1)


if __name__ == "__main__":
    main()
