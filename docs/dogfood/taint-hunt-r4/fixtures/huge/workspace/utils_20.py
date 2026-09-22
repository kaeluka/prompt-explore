import logging

log = logging.getLogger(__name__)


def helper_20(value):
    return str(value).strip()


def compute_20(items):
    return sum(int(x) for x in items)
