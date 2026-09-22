import logging

log = logging.getLogger(__name__)


def helper_26(value):
    return str(value).strip()


def compute_26(items):
    return sum(int(x) for x in items)
