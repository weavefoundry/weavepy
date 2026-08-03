"""Ecosystem probe: rich — console render into a captured buffer, table
layout, markup, and traceback formatting (exercises markdown-it-py and
pygments)."""

import io

import rich
from rich.console import Console
from rich.table import Table
from rich.traceback import Traceback

# render into a buffer, not a tty
buf = io.StringIO()
console = Console(file=buf, width=60, force_terminal=False)

console.print("[bold]hello[/bold] [red]world[/red]")
out = buf.getvalue()
assert "hello" in out and "world" in out, out

# table: headers + rows land in the rendered grid
buf.truncate(0)
buf.seek(0)
table = Table(title="langs")
table.add_column("name")
table.add_column("year")
table.add_row("python", "1991")
table.add_row("rust", "2015")
console.print(table)
out = buf.getvalue()
for needle in ("langs", "name", "year", "python", "1991", "rust", "2015"):
    assert needle in out, (needle, out)

# traceback rendering names the exception and the raising frame
try:
    raise ValueError("probe-boom")
except ValueError:
    buf.truncate(0)
    buf.seek(0)
    console.print(Traceback())
    out = buf.getvalue()
    assert "ValueError" in out and "probe-boom" in out, out

# syntax highlighting via pygments
from rich.syntax import Syntax

buf.truncate(0)
buf.seek(0)
console.print(Syntax("def f():\n    return 1\n", "python"))
assert "def f" in buf.getvalue()

# rich 15 dropped the module-level `__version__`; read the wheel metadata.
import importlib.metadata

print("rich ok", importlib.metadata.version("rich"))
