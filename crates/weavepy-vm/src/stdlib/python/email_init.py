"""WeavePy `email` package — top-level wrapper.

Re-exports the most commonly used helpers from submodules.
"""

from email.message import Message, EmailMessage
from email.parser import Parser, BytesParser, FeedParser
import email.utils as utils


def message_from_string(s, _class=Message, *, policy=None):
    return Parser(_class, policy=policy).parsestr(s)


def message_from_bytes(b, _class=Message, *, policy=None):
    return BytesParser(_class, policy=policy).parsebytes(b)


def message_from_file(fp, _class=Message, *, policy=None):
    return Parser(_class, policy=policy).parse(fp)


def message_from_binary_file(fp, _class=Message, *, policy=None):
    return BytesParser(_class, policy=policy).parse(fp)


__all__ = [
    "Message", "EmailMessage",
    "Parser", "BytesParser", "FeedParser",
    "message_from_string", "message_from_bytes",
    "message_from_file", "message_from_binary_file",
    "utils",
]
