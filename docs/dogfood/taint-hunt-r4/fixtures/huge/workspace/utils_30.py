import logging

log = logging.getLogger(__name__)


def helper_30(value):
    return str(value).strip()


def compute_30(items):
    return sum(int(x) for x in items)
