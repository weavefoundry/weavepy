"""Internal support module for sre (deprecated alias for re._constants)."""

import warnings
warnings.warn(f"module {__name__!r} is deprecated",
              DeprecationWarning, stacklevel=2)

from re import _constants
globals().update({k: v for k, v in vars(_constants).items()
                  if not k.startswith('__')})

del warnings, _constants
