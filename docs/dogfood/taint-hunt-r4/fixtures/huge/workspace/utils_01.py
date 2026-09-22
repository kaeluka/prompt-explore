import logging

log = logging.getLogger(__name__)


def helper_01(value):
    return str(value).strip()


def compute_01(items):
    return sum(int(x) for x in items)
