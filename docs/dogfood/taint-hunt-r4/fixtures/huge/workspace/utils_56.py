import logging

log = logging.getLogger(__name__)


def helper_56(value):
    return str(value).strip()


def compute_56(items):
    return sum(int(x) for x in items)
