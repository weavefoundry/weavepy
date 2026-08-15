"""Ecosystem probe: Pillow (PyPI wheel) — draw primitives, geometry ops,
PNG/JPEG codec round-trips, raw byte access, numpy interop, EXIF."""

import io

from PIL import Image, ImageDraw

# Image.new + ImageDraw primitives
img = Image.new("RGB", (64, 48), color=(10, 20, 30))
draw = ImageDraw.Draw(img)
draw.rectangle([4, 4, 20, 20], fill=(255, 0, 0), outline=(0, 255, 0))
draw.line([(0, 0), (63, 47)], fill=(0, 0, 255), width=2)
draw.ellipse([30, 10, 50, 30], fill=(200, 200, 0))
assert img.getpixel((10, 10)) == (255, 0, 0)
assert img.getpixel((0, 47)) == (10, 20, 30)

# resize / rotate / crop
resized = img.resize((32, 24))
assert resized.size == (32, 24)
rotated = img.rotate(90, expand=True)
assert rotated.size == (48, 64)
cropped = img.crop((4, 4, 21, 21))
assert cropped.size == (17, 17)
assert cropped.getpixel((0, 0)) == img.getpixel((4, 4))
assert cropped.getpixel((6, 6)) == (255, 0, 0)  # inside the filled rectangle

# PNG save -> load round-trip (lossless: pixel equality)
png_buf = io.BytesIO()
img.save(png_buf, format="PNG")
png_buf.seek(0)
png_back = Image.open(png_buf)
png_back.load()
assert png_back.size == img.size and png_back.mode == "RGB"
assert list(png_back.getdata()) == list(img.getdata()), "PNG round-trip pixels"

# JPEG save -> load round-trip (lossy: just structural checks)
jpg_buf = io.BytesIO()
img.save(jpg_buf, format="JPEG", quality=90)
jpg_buf.seek(0)
jpg_back = Image.open(jpg_buf)
jpg_back.load()
assert jpg_back.size == img.size and jpg_back.mode == "RGB"

# tobytes / frombytes
raw = img.tobytes()
assert len(raw) == 64 * 48 * 3
rebuilt = Image.frombytes("RGB", img.size, raw)
assert list(rebuilt.getdata()) == list(img.getdata())

# numpy interop over the buffer surface (fromarray / asarray)
import numpy as np

arr = np.asarray(img)
assert arr.shape == (48, 64, 3) and arr.dtype == np.uint8
assert tuple(arr[10, 10]) == (255, 0, 0)
from_arr = Image.fromarray(arr)
assert list(from_arr.getdata()) == list(img.getdata())

gradient = np.arange(32 * 32, dtype=np.uint8).reshape(32, 32)
gray = Image.fromarray(gradient, mode="L")
assert gray.getpixel((5, 3)) == int(gradient[3, 5])

# EXIF read on a synthesized JPEG
exif = Image.Exif()
exif[271] = "WeavePy"  # Make
exif[272] = "probe-camera"  # Model
exif_buf = io.BytesIO()
img.save(exif_buf, format="JPEG", exif=exif)
exif_buf.seek(0)
tagged = Image.open(exif_buf)
read_exif = tagged.getexif()
assert read_exif[271] == "WeavePy", dict(read_exif)
assert read_exif[272] == "probe-camera", dict(read_exif)

import PIL

print("pillow ok", PIL.__version__)
