"""Text wrapping and filling — WeavePy port of CPython's ``textwrap``.

Provides :class:`TextWrapper`, :func:`wrap`, :func:`fill`,
:func:`shorten`, :func:`dedent`, and :func:`indent`.
"""

import re


_whitespace = "\t\n\x0b\x0c\r "


class TextWrapper:
    unicode_whitespace_trans = {ord(c): " " for c in _whitespace}

    # We use the simple whitespace split everywhere. CPython has a
    # fancier regex that splits on hyphenated words too; that
    # variant uses look-around which WeavePy's regex engine doesn't
    # support. Hyphen-aware splitting is a future enhancement.
    wordsep_re = re.compile(r"(\s+)")
    wordsep_simple_re = re.compile(r"(\s+)")

    sentence_end_re = re.compile(r"[a-z][\.\!\?][\"\']?$")

    def __init__(self,
                 width=70,
                 initial_indent="",
                 subsequent_indent="",
                 expand_tabs=True,
                 replace_whitespace=True,
                 fix_sentence_endings=False,
                 break_long_words=True,
                 drop_whitespace=True,
                 break_on_hyphens=True,
                 tabsize=8,
                 max_lines=None,
                 placeholder=" [...]"):
        self.width = width
        self.initial_indent = initial_indent
        self.subsequent_indent = subsequent_indent
        self.expand_tabs = expand_tabs
        self.replace_whitespace = replace_whitespace
        self.fix_sentence_endings = fix_sentence_endings
        self.break_long_words = break_long_words
        self.drop_whitespace = drop_whitespace
        self.break_on_hyphens = break_on_hyphens
        self.tabsize = tabsize
        self.max_lines = max_lines
        self.placeholder = placeholder

    def _munge_whitespace(self, text):
        if self.expand_tabs:
            text = text.expandtabs(self.tabsize)
        if self.replace_whitespace:
            text = text.translate(self.unicode_whitespace_trans)
        return text

    def _split_chunks(self, text):
        text = self._munge_whitespace(text)
        if self.break_on_hyphens:
            chunks = self.wordsep_re.split(text)
        else:
            chunks = self.wordsep_simple_re.split(text)
        return [c for c in chunks if c]

    def _fix_sentence_endings(self, chunks):
        i = 0
        patsearch = self.sentence_end_re.search
        while i < len(chunks) - 1:
            if chunks[i + 1] == " " and patsearch(chunks[i]):
                chunks[i + 1] = "  "
                i += 2
            else:
                i += 1

    def _handle_long_word(self, reversed_chunks, cur_line, cur_len, width):
        if width < 1:
            space_left = 1
        else:
            space_left = width - cur_len
        if self.break_long_words:
            cur_line.append(reversed_chunks[-1][:space_left])
            reversed_chunks[-1] = reversed_chunks[-1][space_left:]
        elif not cur_line:
            cur_line.append(reversed_chunks.pop())

    def _wrap_chunks(self, chunks):
        lines = []
        if self.width <= 0:
            raise ValueError("invalid width %r (must be > 0)" % self.width)
        chunks.reverse()
        while chunks:
            cur_line = []
            cur_len = 0
            if lines:
                indent = self.subsequent_indent
            else:
                indent = self.initial_indent
            width = self.width - len(indent)
            if self.drop_whitespace and chunks[-1].strip() == "" and lines:
                del chunks[-1]
            while chunks:
                length = len(chunks[-1])
                if cur_len + length <= width:
                    cur_line.append(chunks.pop())
                    cur_len += length
                else:
                    break
            if chunks and len(chunks[-1]) > width:
                self._handle_long_word(chunks, cur_line, cur_len, width)
                cur_len = sum(map(len, cur_line))
            if self.drop_whitespace and cur_line and cur_line[-1].strip() == "":
                cur_len -= len(cur_line[-1])
                del cur_line[-1]
            if cur_line:
                lines.append(indent + "".join(cur_line))
        return lines

    def wrap(self, text):
        chunks = self._split_chunks(text)
        if self.fix_sentence_endings:
            self._fix_sentence_endings(chunks)
        return self._wrap_chunks(chunks)

    def fill(self, text):
        return "\n".join(self.wrap(text))


def wrap(text, width=70, **kwargs):
    return TextWrapper(width=width, **kwargs).wrap(text)


def fill(text, width=70, **kwargs):
    return TextWrapper(width=width, **kwargs).fill(text)


def shorten(text, width, **kwargs):
    w = TextWrapper(width=width, max_lines=1, **kwargs)
    return w.fill(" ".join(text.strip().split()))


_whitespace_only_re = re.compile("^[ \t]+$", re.MULTILINE)
_leading_whitespace_re = re.compile("(^[ \t]*)(?:[^ \t\n])", re.MULTILINE)


def dedent(text):
    lines = text.split("\n")
    margins = []
    for line in lines:
        stripped = line.lstrip()
        if not stripped:
            continue
        margin = line[:len(line) - len(stripped)]
        margins.append(margin)
    if not margins:
        return text
    margin = margins[0]
    for m in margins[1:]:
        for i, c in enumerate(margin):
            if i >= len(m) or m[i] != c:
                margin = margin[:i]
                break
        else:
            margin = margin[:len(m)]
    if not margin:
        return text
    return "\n".join(line[len(margin):] if line.startswith(margin) else line.lstrip()
                     for line in lines)


def indent(text, prefix, predicate=None):
    if predicate is None:
        def predicate(line):
            return line.strip()
    def prefixed_lines():
        for line in text.splitlines(True):
            yield (prefix + line if predicate(line) else line)
    return "".join(prefixed_lines())


__all__ = ["TextWrapper", "wrap", "fill", "shorten", "dedent", "indent"]
