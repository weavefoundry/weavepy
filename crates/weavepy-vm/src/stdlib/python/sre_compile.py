"""Internal support module for sre (deprecated alias for re._compiler)."""

import warnings
warnings.warn(f"module {__name__!r} is deprecated",
              DeprecationWarning, stacklevel=2)

from re import _compiler
globals().update({k: v for k, v in vars(_compiler).items()
                  if not k.startswith('__')})

del warnings, _compiler
