"""WeavePy's pure-Python ``argparse`` lite.

Implements the slice of ``argparse`` actually used by 95% of CLI
scripts: ``ArgumentParser``, ``add_argument`` for positional and
optional arguments, ``parse_args``, ``--help``, the ``action``
shortcuts ``store``, ``store_true``, ``store_false``, ``append``,
``count``, and the ``Namespace`` result object.
"""

import sys


__all__ = ["ArgumentParser", "Namespace", "ArgumentError"]


class ArgumentError(Exception):
    pass


class Namespace:
    def __init__(self, **kwargs):
        for k, v in kwargs.items():
            setattr(self, k, v)

    def __repr__(self):
        attrs = []
        for k, v in vars(self).items():
            attrs.append(k + "=" + repr(v))
        return "Namespace(" + ", ".join(attrs) + ")"

    def __eq__(self, other):
        if isinstance(other, Namespace):
            return vars(self) == vars(other)
        return NotImplemented


class _Action:
    def __init__(
        self,
        flags,
        dest,
        nargs,
        default,
        type,
        choices,
        required,
        help,
        action,
        const,
    ):
        self.flags = flags
        self.dest = dest
        self.nargs = nargs
        self.default = default
        self.type = type
        self.choices = choices
        self.required = required
        self.help = help
        self.action = action
        self.const = const

    @property
    def is_optional(self):
        return any(f.startswith("-") for f in self.flags)


def _flag_to_dest(flag):
    return flag.lstrip("-").replace("-", "_")


class _ArgumentGroup:
    """Lightweight stand-in for ``argparse``'s argument groups: forwards
    ``add_argument`` to the owning parser so grouped options participate in
    normal parsing (we don't render the per-group help sections)."""

    def __init__(self, container, title=None, description=None):
        self._container = container
        self.title = title
        self.description = description

    def add_argument(self, *flags, **kwargs):
        return self._container.add_argument(*flags, **kwargs)

    def add_argument_group(self, *args, **kwargs):
        return _ArgumentGroup(self._container)

    def add_mutually_exclusive_group(self, **kwargs):
        return _ArgumentGroup(self._container)


class ArgumentParser:
    def __init__(
        self,
        prog=None,
        description=None,
        epilog=None,
        add_help=True,
    ):
        self.prog = prog or (sys.argv[0] if sys.argv else "prog")
        self.description = description
        self.epilog = epilog
        self._actions = []
        if add_help:
            self.add_argument(
                "-h",
                "--help",
                action="help",
                help="show this help message and exit",
            )

    def add_argument(self, *flags, **kwargs):
        action = kwargs.get("action", "store")
        nargs = kwargs.get("nargs", None)
        default = kwargs.get("default", None)
        type_ = kwargs.get("type", str)
        choices = kwargs.get("choices", None)
        required = kwargs.get("required", False)
        help_text = kwargs.get("help", None)
        const = kwargs.get("const", None)
        dest = kwargs.get("dest", None)

        if dest is None:
            for flag in flags:
                if flag.startswith("--"):
                    dest = _flag_to_dest(flag)
                    break
            else:
                dest = _flag_to_dest(flags[0])

        if action == "store_true":
            default = False if default is None else default
            const = True
        elif action == "store_false":
            default = True if default is None else default
            const = False
        elif action == "count":
            default = 0 if default is None else default

        a = _Action(
            flags=list(flags),
            dest=dest,
            nargs=nargs,
            default=default,
            type=type_,
            choices=choices,
            required=required,
            help=help_text,
            action=action,
            const=const,
        )
        self._actions.append(a)
        return a

    def add_argument_group(self, *args, **kwargs):
        # Argument groups only affect help grouping in CPython; their
        # arguments live in the parent parser's action list. A thin proxy
        # that forwards `add_argument` is enough for parsing parity.
        return _ArgumentGroup(self)

    def add_mutually_exclusive_group(self, **kwargs):
        return _ArgumentGroup(self)

    def _flag_action(self, token):
        for action in self._actions:
            for flag in action.flags:
                if flag == token:
                    return action
                if flag.startswith("--") and token.startswith(flag + "="):
                    return action
        return None

    def _convert(self, action, value):
        """Apply ``action.type`` to ``value``, converting a failed
        conversion into argparse's ``usage:`` + ``error:`` exit (SystemExit),
        exactly like CPython's ``_get_value``."""
        if action.type is None or value is None:
            return value
        try:
            return action.type(value)
        except (ValueError, TypeError):
            name = action.flags[0] if action.flags else action.dest
            type_name = getattr(action.type, "__name__", repr(action.type))
            self.error(
                "argument %s: invalid %s value: %r" % (name, type_name, value)
            )

    def _apply_action(self, action, value, namespace):
        if action.choices is not None and value not in action.choices:
            self.error(
                "argument "
                + action.flags[0]
                + ": invalid choice: "
                + repr(value)
            )
        if action.action == "store":
            converted = self._convert(action, value)
            setattr(namespace, action.dest, converted)
        elif action.action == "append":
            existing = getattr(namespace, action.dest, None)
            if existing is None:
                existing = []
            converted = self._convert(action, value)
            existing.append(converted)
            setattr(namespace, action.dest, existing)

    def _set_defaults(self, namespace):
        for action in self._actions:
            if not hasattr(namespace, action.dest):
                setattr(namespace, action.dest, action.default)

    def parse_args(self, args=None, namespace=None):
        if args is None:
            args = sys.argv[1:]
        if namespace is None:
            namespace = Namespace()
        self._set_defaults(namespace)

        positionals = [a for a in self._actions if not a.is_optional]
        positional_values = []

        i = 0
        seen_optionals = set()
        while i < len(args):
            token = args[i]
            if token == "--":
                positional_values.extend(args[i + 1:])
                break
            if token.startswith("-"):
                action = self._flag_action(token)
                if action is None:
                    self.error("unrecognised arguments: " + token)
                seen_optionals.add(action.dest)
                if action.action == "help":
                    self.print_help()
                    sys.exit(0)
                elif action.action == "store_true" or action.action == "store_false":
                    setattr(namespace, action.dest, action.const)
                    i += 1
                    continue
                elif action.action == "count":
                    current = getattr(namespace, action.dest, 0) or 0
                    setattr(namespace, action.dest, current + 1)
                    i += 1
                    continue
                if "=" in token and token.startswith("--"):
                    value = token.split("=", 1)[1]
                    i += 1
                else:
                    if i + 1 >= len(args):
                        self.error(
                            "argument " + token + ": expected one argument"
                        )
                    value = args[i + 1]
                    i += 2
                self._apply_action(action, value, namespace)
            else:
                positional_values.append(token)
                i += 1

        for action in positionals:
            nargs = action.nargs
            if nargs in (None, 1):
                if not positional_values:
                    if action.default is not None:
                        setattr(namespace, action.dest, action.default)
                        continue
                    self.error(
                        "the following arguments are required: " + action.dest
                    )
                value = positional_values.pop(0)
                self._apply_action(action, value, namespace)
            elif nargs == "*":
                values = [self._convert(action, v) for v in positional_values]
                positional_values = []
                setattr(namespace, action.dest, values)
            elif nargs == "+":
                if not positional_values:
                    self.error(
                        "the following arguments are required: " + action.dest
                    )
                values = [self._convert(action, v) for v in positional_values]
                positional_values = []
                setattr(namespace, action.dest, values)
            elif nargs == "?":
                if positional_values:
                    value = positional_values.pop(0)
                    self._apply_action(action, value, namespace)
                else:
                    setattr(namespace, action.dest, action.default)
            elif isinstance(nargs, int):
                if len(positional_values) < nargs:
                    self.error("expected " + str(nargs) + " arguments for " + action.dest)
                taken = positional_values[:nargs]
                positional_values = positional_values[nargs:]
                values = [self._convert(action, v) for v in taken]
                setattr(namespace, action.dest, values)
            else:
                self.error("invalid nargs value for " + action.dest)

        for action in self._actions:
            if action.required and action.dest not in seen_optionals:
                self.error("required argument missing: " + action.flags[0])

        if positional_values:
            self.error("unrecognised arguments: " + " ".join(positional_values))

        return namespace

    def parse_known_args(self, args=None, namespace=None):
        """Same as :meth:`parse_args`, but tolerates extra arguments.

        Returns ``(namespace, remaining_args)``. Used by stdlib modules
        like ``pdb`` and ``unittest`` that want to strip off the flags
        they understand and forward the rest to user code.
        """
        if args is None:
            args = list(sys.argv[1:])
        else:
            args = list(args)
        if namespace is None:
            namespace = Namespace()
        self._set_defaults(namespace)

        consumed = [False] * len(args)
        seen_optionals = set()
        positional_values = []

        # Two passes: first collect known options, then assign
        # positionals. This mirrors CPython's behaviour where unknowns
        # get returned untouched.
        i = 0
        while i < len(args):
            token = args[i]
            if token == "--":
                # Everything after is positional.
                consumed[i] = True
                for j in range(i + 1, len(args)):
                    if not consumed[j]:
                        positional_values.append(args[j])
                        consumed[j] = True
                break
            if token.startswith("-"):
                action = self._flag_action(token)
                if action is None:
                    # Unknown flag — leave for caller.
                    i += 1
                    continue
                consumed[i] = True
                seen_optionals.add(action.dest)
                if action.action == "help":
                    self.print_help()
                    sys.exit(0)
                elif action.action == "store_true" or action.action == "store_false":
                    setattr(namespace, action.dest, action.const)
                    i += 1
                    continue
                elif action.action == "count":
                    current = getattr(namespace, action.dest, 0) or 0
                    setattr(namespace, action.dest, current + 1)
                    i += 1
                    continue
                if "=" in token and token.startswith("--"):
                    value = token.split("=", 1)[1]
                    i += 1
                else:
                    if i + 1 >= len(args):
                        self.error("argument " + token + ": expected one argument")
                    value = args[i + 1]
                    consumed[i + 1] = True
                    i += 2
                self._apply_action(action, value, namespace)
            else:
                i += 1

        for k, was in enumerate(consumed):
            if not was and not args[k].startswith("-"):
                positional_values.append(args[k])
                consumed[k] = True

        positionals = [a for a in self._actions if not a.is_optional]
        for action in positionals:
            nargs = action.nargs
            if nargs in (None, 1):
                if positional_values:
                    value = positional_values.pop(0)
                    self._apply_action(action, value, namespace)
                else:
                    setattr(namespace, action.dest, action.default)
            elif nargs == "*":
                values = [self._convert(action, v) for v in positional_values]
                positional_values = []
                setattr(namespace, action.dest, values)
            elif nargs == "+":
                values = [self._convert(action, v) for v in positional_values]
                positional_values = []
                setattr(namespace, action.dest, values)
            elif nargs == "?":
                if positional_values:
                    value = positional_values.pop(0)
                    self._apply_action(action, value, namespace)
                else:
                    setattr(namespace, action.dest, action.default)
            elif isinstance(nargs, int):
                taken = positional_values[:nargs]
                positional_values = positional_values[nargs:]
                values = [self._convert(action, v) for v in taken]
                setattr(namespace, action.dest, values)

        # Anything still in `args` that wasn't consumed is "unknown".
        remaining = [args[k] for k in range(len(args)) if not consumed[k]]
        remaining.extend(positional_values)
        return namespace, remaining

    def format_help(self):
        lines = []
        if self.description:
            lines.append(self.description)
            lines.append("")
        lines.append("usage: " + self.prog + " [options]")
        lines.append("")
        for action in self._actions:
            flag_str = ", ".join(action.flags)
            lines.append("  " + flag_str + "  " + (action.help or ""))
        if self.epilog:
            lines.append("")
            lines.append(self.epilog)
        return "\n".join(lines)

    def format_usage(self):
        return "usage: " + self.prog + " [options]\n"

    def print_usage(self, file=None):
        if file is None:
            file = sys.stdout
        file.write(self.format_usage())

    def print_help(self, file=None):
        if file is None:
            file = sys.stdout
        file.write(self.format_help())
        file.write("\n")

    def error(self, message):
        # CPython prints the usage line before the error, then exits 2.
        self.print_usage(sys.stderr)
        sys.stderr.write(self.prog + ": error: " + message + "\n")
        sys.exit(2)
