import logging

log = logging.getLogger(__name__)


def helper_07(value):
    return str(value).strip()


def compute_07(items):
    return sum(int(x) for x in items)
