"""Ecosystem probe: matplotlib (PyPI wheel), Agg backend only — renders
a line+scatter figure with labels to PNG through FigureCanvasAgg,
asserts dimensions and non-blank pixel statistics, and round-trips the
PNG through Pillow. Exercises ft2font, kiwisolver, and the RFC 0066
WS1/WS3 buffer surfaces together."""

import io
import os

os.environ["MPLBACKEND"] = "Agg"

import matplotlib

matplotlib.use("Agg")

import numpy as np
from matplotlib.backends.backend_agg import FigureCanvasAgg
from matplotlib.figure import Figure

fig = Figure(figsize=(4, 3), dpi=100)
canvas = FigureCanvasAgg(fig)
ax = fig.add_subplot(1, 1, 1)

x = np.linspace(0.0, 2.0 * np.pi, 50)
ax.plot(x, np.sin(x), color="tab:blue", linewidth=2, label="sin")
ax.scatter(x[::5], np.cos(x[::5]), color="tab:red", marker="o", label="cos samples")
ax.set_title("WeavePy probe")
ax.set_xlabel("x")
ax.set_ylabel("y")
ax.legend(loc="upper right")

canvas.draw()

# Raw ARGB buffer: correct dimensions, and actually drawn-on (a blank
# canvas is all-white — the plot must produce non-white pixels and more
# than a handful of distinct values).
width, height = canvas.get_width_height()
assert (width, height) == (400, 300), (width, height)
rgba = np.asarray(canvas.buffer_rgba())
assert rgba.shape == (300, 400, 4), rgba.shape
nonwhite = int((rgba[..., :3] != 255).any(axis=-1).sum())
assert nonwhite > 1000, f"canvas nearly blank: {nonwhite} non-white pixels"
assert len(np.unique(rgba[..., :3])) > 10, "suspiciously flat pixel statistics"

# PNG encode -> Pillow decode round-trip.
png_buf = io.BytesIO()
fig.savefig(png_buf, format="png")
assert png_buf.getvalue()[:8] == b"\x89PNG\r\n\x1a\n"
png_buf.seek(0)

from PIL import Image

img = Image.open(png_buf)
img.load()
assert img.size == (400, 300) and img.mode in ("RGB", "RGBA"), (img.size, img.mode)
arr = np.asarray(img.convert("RGB"))
assert int((arr != 255).any(axis=-1).sum()) > 1000, "PNG decoded nearly blank"

print("matplotlib ok", matplotlib.__version__)
