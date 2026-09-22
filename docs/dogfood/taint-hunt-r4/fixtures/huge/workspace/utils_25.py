import logging

log = logging.getLogger(__name__)


def helper_25(value):
    return str(value).strip()


def compute_25(items):
    return sum(int(x) for x in items)
