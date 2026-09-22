import logging

log = logging.getLogger(__name__)


def helper_16(value):
    return str(value).strip()


def compute_16(items):
    return sum(int(x) for x in items)
