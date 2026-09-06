"""Shim of CPython 3.14's `_remote_debugging` (`Modules/_remote_debugging_module.c`).

The real module reads another process's memory to unwind its Python
stacks. That transport (`process_vm_readv`, `mach_vm_read`,
`ReadProcessMemory`) has no equivalent in this runtime, so
`RemoteUnwinder(...)` raises the same `PermissionError`/`RuntimeError`
shape the C module raises when it can't attach. The struct-sequence
result types are here in full: `asyncio.tools` imports `FrameInfo` at
module load time (so `python -m asyncio` needs them), and the
`asyncio ps`/`pstree` commands build and compare them.
"""

import sys

__all__ = [
    "AwaitedInfo",
    "CoroInfo",
    "FrameInfo",
    "PROCESS_VM_READV_SUPPORTED",
    "RemoteUnwinder",
    "TaskInfo",
    "ThreadInfo",
]

# The C module sets this from `HAVE_PROCESS_VM_READV` (Linux-only).
PROCESS_VM_READV_SUPPORTED = False


class _StructSequence(tuple):
    """`PyStructSequence` as a tuple subclass: fixed arity, named
    read-only fields, and CPython's `module.Name(field=value, ...)` repr."""

    __slots__ = ()
    _fields = ()
    __module__ = "_remote_debugging"

    def __new__(cls, sequence=(), dict=None):
        items = tuple(sequence)
        n = len(cls._fields)
        if len(items) != n:
            kind = "at most" if len(items) > n else "at least"
            raise TypeError(
                f"{cls.__module__}.{cls.__name__}() takes an {kind} {n}-sequence "
                f"({len(items)}-sequence given)"
            )
        return tuple.__new__(cls, items)

    def __repr__(self):
        body = ", ".join(f"{name}={value!r}" for name, value in zip(self._fields, self))
        return f"{self.__module__}.{type(self).__name__}({body})"

    def __reduce__(self):
        return (type(self), (tuple(self),))

    def __replace__(self, /, **kwargs):
        values = dict(zip(self._fields, self))
        for name in kwargs:
            if name not in values:
                raise TypeError(
                    f"{type(self).__name__}.__replace__() got an unexpected "
                    f"keyword argument '{name}'"
                )
        values.update(kwargs)
        return type(self)(values.values())


def _field(index, doc=None):
    return property(lambda self: tuple.__getitem__(self, index), doc=doc)


def _struct_sequence(name, doc, fields):
    ns = {
        "__slots__": (),
        "__doc__": doc,
        "_fields": tuple(f for f, _ in fields),
        "n_fields": len(fields),
        "n_sequence_fields": len(fields),
        "n_unnamed_fields": 0,
    }
    for index, (field, fdoc) in enumerate(fields):
        ns[field] = _field(index, fdoc)
    return type(name, (_StructSequence,), ns)


FrameInfo = _struct_sequence(
    "FrameInfo",
    "Information about a frame",
    [
        ("filename", "Source code filename"),
        ("lineno", "Line number"),
        ("funcname", "Function name"),
    ],
)

CoroInfo = _struct_sequence(
    "CoroInfo",
    "Information about a coroutine",
    [
        ("call_stack", "Call stack of the coroutine"),
        ("task_name", "Name of the task"),
    ],
)

TaskInfo = _struct_sequence(
    "TaskInfo",
    "Information about an asyncio task",
    [
        ("task_id", "Task ID (memory address)"),
        ("task_name", "Task name"),
        ("coroutine_stack", "Coroutine call stack"),
        ("awaited_by", "Tasks awaiting this task"),
    ],
)

AwaitedInfo = _struct_sequence(
    "AwaitedInfo",
    "Information about what a thread is awaiting",
    [
        ("thread_id", "Thread ID"),
        ("awaited_by", "List of tasks being awaited"),
    ],
)

ThreadInfo = _struct_sequence(
    "ThreadInfo",
    "Information about a thread",
    [
        ("thread_id", "Thread ID"),
        ("frame_info", "Frame information"),
    ],
)


class RemoteUnwinder:
    """RemoteUnwinder(pid, *, all_threads=False, only_active_thread=False, debug=False)

    Initialize a new RemoteUnwinder object for debugging a remote Python process.

    Args:
        pid: Process ID of the target Python process to debug
        all_threads: If True, initialize state for all threads in the process.
                    If False, only initialize for the main thread.
        only_active_thread: If True, only sample the thread holding the GIL.
                           Cannot be used together with all_threads=True.
        debug: If True, chain exceptions to explain the sequence of events that
               lead to the exception.

    The Python object is created and destroyed on each call. Attaching to
    another process's address space isn't available in this runtime, so
    construction fails the way CPython's does without attach permission.
    """

    def __init__(self, pid, *, all_threads=False, only_active_thread=False, debug=False):
        if all_threads and only_active_thread:
            raise ValueError("all_threads and only_active_thread cannot both be true")
        pid = int(pid)
        if sys.platform == "darwin":
            raise PermissionError(
                f"Cannot get task port for PID {pid} (kern_return_t: 5). This "
                "typically requires running as root or having the "
                "'com.apple.system-task-ports' entitlement."
            )
        if sys.platform == "linux":
            raise PermissionError(
                f"Cannot access process memory for PID {pid}: this runtime "
                "has no remote memory reader"
            )
        raise RuntimeError("Remote debugging is not supported on this platform")

    def get_stack_trace(self):
        raise RuntimeError("RemoteUnwinder is not attached to a process")

    def get_all_awaited_by(self):
        raise RuntimeError("RemoteUnwinder is not attached to a process")

    def get_async_stack_trace(self):
        raise RuntimeError("RemoteUnwinder is not attached to a process")
