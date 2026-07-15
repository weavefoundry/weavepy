"""_tokenize — WeavePy's port of CPython's C accelerator (RFC 0052).

CPython's ``_tokenize.TokenizerIter`` drives the readline flavour of the
pegen tokenizer lazily. WeavePy's native lexer port lives in the
``_tokenize_core`` builtin, which tokenizes a *slurped* list of source
lines in one call; this shim keeps the readline dispatch (str/bytes type
checks, per-line decoding with ``errors="replace"``) in Python, then
yields the precomputed 5-tuples and re-raises any tokenization error
exactly where CPython would — after the tokens that precede it.
"""

import _tokenize_core

__all__ = ["TokenizerIter"]


class TokenizerIter:
    """Iterator of raw token 5-tuples over a readline callable.

    Mirrors ``_tokenize.TokenizerIter(readline, *, extra_tokens,
    encoding='utf-8')``: with an *encoding*, ``readline()`` must return
    bytes (decoded per line, like ``tok_readline_string``); without one
    it must return str. ``StopIteration`` or an empty line signals EOF.
    """

    def __init__(self, readline, /, *, extra_tokens, encoding=None):
        self._readline = readline
        self._encoding = encoding
        self._extra_tokens = bool(extra_tokens)
        self._tokens = None
        self._index = 0
        self._error = None

    def __iter__(self):
        return self

    def _tokenize(self):
        lines = []
        readline = self._readline
        encoding = self._encoding
        while True:
            try:
                line = readline()
            except StopIteration:
                break
            if encoding is not None:
                if not isinstance(line, bytes):
                    raise TypeError("readline() returned a non-bytes object")
                line = line.decode(encoding, "replace")
            else:
                if not isinstance(line, str):
                    raise TypeError("readline() returned a non-string object")
            if not line:
                break
            lines.append(line)
        self._tokens, self._error = _tokenize_core.tokens(
            lines, self._extra_tokens
        )

    def __next__(self):
        if self._tokens is None:
            self._tokenize()
        if self._index < len(self._tokens):
            tok = self._tokens[self._index]
            self._index += 1
            return tok
        if self._error is not None:
            kind, msg, lineno, offset, text, end_lineno, end_offset = self._error
            self._error = None
            if kind == "indent":
                exc_type = IndentationError
            elif kind == "tab":
                exc_type = TabError
            else:
                exc_type = SyntaxError
            if text is None:
                # The bare-location E_EOF flavour
                # (PyErr_SyntaxLocationObject): no source text attached.
                exc = exc_type(msg)
                exc.filename = "<string>"
                exc.lineno = lineno
                exc.offset = offset
                raise exc
            raise exc_type(
                msg, ("<string>", lineno, offset, text, end_lineno, end_offset)
            )
        raise StopIteration("EOF")
