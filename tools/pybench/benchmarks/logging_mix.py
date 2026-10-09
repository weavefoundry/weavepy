"""logging: filtered-out calls, formatted records to a StringIO handler,
and %-style arguments (after pyperformance's `logging`)."""

import io
import logging

WORK = 4000


def bench(n):
    stream = io.StringIO()
    handler = logging.StreamHandler(stream)
    handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))
    logger = logging.getLogger("bench.%d" % n)
    logger.handlers[:] = [handler]
    logger.propagate = False
    logger.setLevel(logging.WARNING)
    for i in range(n):
        logger.debug("silent %d %s", i, "x")
        logger.info("also silent %s", i)
        logger.warning("warning number %d of %s", i, "items")
        if i % 4 == 0:
            logger.error("error: %r", {"i": i})
    lines = stream.getvalue().count("\n")
    logger.handlers[:] = []
    return lines
