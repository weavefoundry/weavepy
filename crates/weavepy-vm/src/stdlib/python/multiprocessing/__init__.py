#
# Package analogous to 'threading.py' but using processes
#
# multiprocessing/__init__.py
#
# This package is intended to duplicate the functionality (and much of
# the API) of threading.py but uses processes instead of threads.  A
# subpackage 'multiprocessing.dummy' has the same API but is a simple
# wrapper for 'threading'.
#
# Copyright (c) 2006-2008, R Oudkerk
# Licensed to PSF under a Contributor Agreement.
#

import sys
from . import context

#
# Copy stuff from default context
#

__all__ = [x for x in dir(context._default_context) if not x.startswith('_')]
globals().update((name, getattr(context._default_context, name)) for name in __all__)

#
# XXX These should not really be documented or public.
#

SUBDEBUG = 5
SUBWARNING = 25

#
# Alias for main module -- will be reset by bootstrapping child processes
#

if '__main__' in sys.modules:
    sys.modules['__mp_main__'] = sys.modules['__main__']

#
# WeavePy `--multiprocessing-fork` child entry point.
#
# The vendored `popen_spawn_posix`/`popen_forkserver` Popen re-exec
# `weavepy --multiprocessing-fork tracker_fd=<N> pipe_handle=<M> ...` via
# `_posixsubprocess.fork_exec`. The Rust CLI detects that argv and calls this
# function; it mirrors the POSIX body of `spawn.spawn_main()` but *returns* the
# child's exit code instead of `sys.exit`-ing, so the CLI bridge controls the
# process status. This is WeavePy's analogue of CPython's frozen
# `spawn.freeze_support()` path.
#

def _run_spawn_child():
    import os
    from . import spawn
    assert spawn.is_forking(sys.argv), 'not a multiprocessing fork child'
    kwds = {}
    for arg in sys.argv[2:]:
        name, value = arg.split('=', 1)
        kwds[name] = None if value == 'None' else int(value)
    from . import resource_tracker
    resource_tracker._resource_tracker._fd = kwds.get('tracker_fd')
    fd = kwds['pipe_handle']
    parent_sentinel = os.dup(fd)
    return spawn._main(fd, parent_sentinel)
