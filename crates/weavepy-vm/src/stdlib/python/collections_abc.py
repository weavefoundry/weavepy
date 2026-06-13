"""``collections.abc`` — re-export of the ABCs defined in ``_collections_abc``.

Carried verbatim from CPython 3.13. The implementation lives in
``_collections_abc`` (so the early-startup machinery can import the few
ABCs it needs without dragging in the whole ``collections`` package); this
module is the public spelling everyone actually imports.
"""

from _collections_abc import *
from _collections_abc import __all__  # noqa: F401
