import logging

log = logging.getLogger(__name__)


def helper_23(value):
    return str(value).strip()


def compute_23(items):
    return sum(int(x) for x in items)
