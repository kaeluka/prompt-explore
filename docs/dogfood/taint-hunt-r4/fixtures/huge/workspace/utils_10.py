import logging

log = logging.getLogger(__name__)


def helper_10(value):
    return str(value).strip()


def compute_10(items):
    return sum(int(x) for x in items)
