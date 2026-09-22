import logging

log = logging.getLogger(__name__)


def helper_36(value):
    return str(value).strip()


def compute_36(items):
    return sum(int(x) for x in items)
