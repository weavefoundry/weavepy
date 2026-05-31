"""Internal support module for sre (deprecated alias for re._parser)."""

import warnings
warnings.warn(f"module {__name__!r} is deprecated",
              DeprecationWarning, stacklevel=2)

from re import _parser
globals().update({k: v for k, v in vars(_parser).items()
                  if not k.startswith('__')})

del warnings, _parser
