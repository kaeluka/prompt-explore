import logging

log = logging.getLogger(__name__)


def helper_06(value):
    return str(value).strip()


def compute_06(items):
    return sum(int(x) for x in items)
