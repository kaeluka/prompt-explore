import logging

log = logging.getLogger(__name__)


def helper_35(value):
    return str(value).strip()


def compute_35(items):
    return sum(int(x) for x in items)
